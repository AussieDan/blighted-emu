// SPDX-License-Identifier: AGPL-3.0-or-later
//! The company treasury. A transfer moves coins in two steps: through the player's sim
//! ([`Effect::MoveCoins`]) and in the database ([`Effect::Treasury`]); coins never exist in
//! neither place without a record of them.
//!
//! - A deposit takes the coins from the player first, then adds them to the treasury. When the
//!   treasury does not take them, they go back to the player.
//! - A withdrawal takes the coins from the treasury first, then gives them to the player. When
//!   the player does not get them, the treasury takes them back.
//! - Coins that can reach neither the player nor the treasury (the player left, its sim refused
//!   them, the company is gone) become a debt to the player in the database (world migration
//!   0031); the player gets them when it is next online.
//! - The daily withdrawal limit counts the withdrawals that reached the player, and the ones on
//!   their way.
//!
//! Answers (Observed: a withdrawal gets 5200 with `TREASURY_SUCCESS`, a deposit 5201 with its
//! amount). A refused withdrawal gets 5200 with the client's `GuildTreasuryResponse` reason
//! (Inferred values, see `nw_messages::social::guilds`). A refused deposit gets 5201 with amount
//! 0 (Inferred: 5201 carries no reason, and no capture holds a refused deposit).
//!
//! Each coin move has an id ([`CoinTransfer::id`]): the service asks the same sim again with the
//! same id when no answer came, and the sim moves the coins of one id once, so a lost or late
//! answer never moves coins twice. A move still unanswered after every attempt (its sim is gone)
//! counts as not done, and the coins go the way that loses none (Inferred): an unanswered
//! deposit becomes a debt to the player, an unanswered withdrawal goes back to the treasury. Only
//! a sim that moved the coins, saved the player and stopped before any answer can so give coins
//! twice.

use nw_bus::{CoinReply, CoinTransfer};
use nw_db::{CoinReturn, TreasuryReason, WITHDRAWAL_WINDOW_MS};
use nw_messages::social::guilds::{
    TREASURY_DAILY_LIMIT, TREASURY_NO_PRIVILEGE, TREASURY_NOT_ENOUGH_TREASURY_FUNDS,
    TREASURY_SUCCESS,
};
use nw_messages::social::{GuildPush, SocialPush};
use uuid::Uuid;

use crate::companies::{Privilege, Right, may};
use crate::effects::{Effect, StoreOp, TreasuryOutcome};
use crate::world::{CoinMove, METRIC_CALLS, REFUSED, RESULT_LABEL, SocialWorld, Transfer};

/// `GuildTreasuryResponse` `GuildDataNotFound` (Inferred: first of the enum, see
/// `nw_messages::social::guilds::TREASURY_SUCCESS`): the company is not known.
pub(crate) const TREASURY_NO_COMPANY: u32 = 1;
/// `GuildTreasuryResponse` `GuildMemberDataNotFound` (Inferred): the player is in no company.
pub(crate) const TREASURY_NOT_A_MEMBER: u32 = 2;
/// `GuildTreasuryResponse` `CoinTransferalDisabled` (Inferred): the coins could not move (the
/// player's sim did not take them, or the database refused the change).
pub(crate) const TREASURY_TRANSFER_FAILED: u32 = 8;
/// The 5201 amount of a refused deposit (Inferred: nothing went in).
pub(crate) const NOTHING_DEPOSITED: u64 = 0;
/// Metric: coin moves the sim did not answer in time.
const METRIC_UNANSWERED_MOVES: &str = "social_coin_moves_unanswered_total";

/// How a coin move of the sim ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveResult {
    /// The coins moved.
    Done,
    /// The sim refused it (the player is not there, or has too few coins).
    Refused,
    /// No answer in time: Unknown whether the coins moved.
    Unanswered,
}

fn change_of(amount: u64) -> i64 {
    // Amounts are below the treasury maximum, a BIGINT column.
    i64::try_from(amount).unwrap_or(i64::MAX)
}

impl SocialWorld {
    /// The sim that has `character` now.
    pub(crate) fn sim_of(&self, character: Uuid) -> Option<nw_bus::BusId> {
        self.people
            .get(&character)
            .and_then(|person| person.online.as_ref())
            .map(|online| online.sim.clone())
    }

    fn deposited(&mut self, character: Uuid, amount: u64) {
        let push = GuildPush::Deposited { amount };
        self.push(&[character], SocialPush::Guild(push));
    }

    fn withdrawn(&mut self, character: Uuid, amount: u64, result: u32) {
        let push = GuildPush::Withdrawn { amount, result };
        self.push(&[character], SocialPush::Guild(push));
    }

    /// Asks the player's sim to add `change` coins for `transfer`; false when the player is
    /// offline (nothing was asked). A debt's move has the debt's id, so a debt paid again after a
    /// lost answer or a restart moves its coins once; every other move has a new id.
    fn move_coins(&mut self, transfer: Transfer, character: Uuid, change: i64) -> bool {
        let Some(sim) = self.sim_of(character) else {
            return false;
        };
        let id = match transfer {
            Transfer::Debt(debt) => debt.id,
            _ => Uuid::new_v4(),
        };
        let ticket = self.ticket();
        self.transfers.insert(ticket, transfer);
        self.out.push(Effect::MoveCoins {
            ticket,
            sim,
            transfer: CoinTransfer {
                id,
                character,
                change,
            },
        });
        true
    }

    fn change_treasury(&mut self, transfer: Transfer, coins: CoinMove, change: i64) {
        let reason = match transfer {
            Transfer::Deposit(_) => TreasuryReason::Deposit,
            Transfer::Withdraw(_) => TreasuryReason::Withdraw,
            Transfer::Refund(_) | Transfer::DepositReturn(_) | Transfer::Debt(_) => {
                TreasuryReason::Refund
            }
        };
        let ticket = self.ticket();
        self.transfers.insert(ticket, transfer);
        self.out.push(Effect::Treasury {
            ticket,
            company: coins.company,
            character: coins.character,
            change,
            reason,
        });
    }

    /// Stores `coins` as a debt to its player, paid when it is next online.
    fn owe(&mut self, coins: CoinMove) {
        let debt = CoinReturn {
            id: Uuid::new_v4(),
            character: coins.character,
            company: Some(coins.company),
            amount: coins.amount,
        };
        tracing::warn!(character = %coins.character, amount = coins.amount, "coins owed to a player");
        self.store(StoreOp::AddCoinReturn(debt));
        self.owed.entry(coins.character).or_default().push(debt);
    }

    /// Sends the coins owed to `character` to its sim.
    pub(crate) fn pay_debts(&mut self, character: Uuid) {
        for debt in self.owed.remove(&character).unwrap_or_default() {
            if !self.move_coins(Transfer::Debt(debt), character, change_of(debt.amount)) {
                self.owed.entry(character).or_default().push(debt);
            }
        }
    }

    pub(crate) fn deposit(&mut self, character: Uuid, asked: u64) {
        let Some((company, rank)) = self.company_of(character) else {
            self.deposited(character, NOTHING_DEPOSITED);
            return;
        };
        if !may(rank, Right::Has(Privilege::TreasuryDeposit)) {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            self.deposited(character, NOTHING_DEPOSITED);
            return;
        }
        // `DepositCappedByTreasuryMax`: the deposit stops at the treasury maximum.
        let room = self.companies.get(&company).map_or(0, |state| {
            self.rules
                .companies
                .treasury_max
                .saturating_sub(state.record.treasury)
        });
        let amount = asked.min(room);
        if amount == 0 {
            self.deposited(character, NOTHING_DEPOSITED);
            return;
        }
        let coins = CoinMove {
            character,
            company,
            amount,
        };
        self.move_coins(Transfer::Deposit(coins), character, -change_of(amount));
    }

    /// The coin `character` took from treasuries in the withdrawal window before `now_ms`: its
    /// withdrawals that reached it, less their refunds.
    pub(crate) fn withdrawn_today(&self, character: Uuid, now_ms: u64) -> u64 {
        let taken: i64 = self
            .withdrawals
            .get(&character)
            .into_iter()
            .flatten()
            .filter(|(at, _)| now_ms.saturating_sub(*at) < WITHDRAWAL_WINDOW_MS)
            .map(|(_, change)| -change)
            .sum();
        u64::try_from(taken).unwrap_or_default()
    }

    /// The coin of the withdrawals of `character` on their way to it.
    fn withdrawing(&self, character: Uuid) -> u64 {
        self.transfers
            .values()
            .filter_map(|transfer| match transfer {
                Transfer::Withdraw(coins) if coins.character == character => Some(coins.amount),
                _ => None,
            })
            .sum()
    }

    pub(crate) fn withdraw(&mut self, character: Uuid, amount: u64, now_ms: u64) {
        let Some((company, rank)) = self.company_of(character) else {
            self.withdrawn(character, amount, TREASURY_NOT_A_MEMBER);
            return;
        };
        if !may(rank, Right::Has(Privilege::TreasuryWithdraw)) {
            self.withdrawn(character, amount, TREASURY_NO_PRIVILEGE);
            return;
        }
        let Some((treasury, limit)) = self
            .companies
            .get(&company)
            .map(|state| (state.record.treasury, state.record.withdrawal_limit))
        else {
            self.withdrawn(character, amount, TREASURY_NO_COMPANY);
            return;
        };
        if treasury < amount {
            self.withdrawn(character, amount, TREASURY_NOT_ENOUGH_TREASURY_FUNDS);
            return;
        }
        if !may(rank, Right::Has(Privilege::TreasuryWithdrawUnlimited))
            && let Some(limit) = limit
            && self
                .withdrawn_today(character, now_ms)
                .saturating_add(self.withdrawing(character))
                .saturating_add(amount)
                > limit
        {
            self.withdrawn(character, amount, TREASURY_DAILY_LIMIT);
            return;
        }
        if amount == 0 {
            // Nothing to move: the withdrawal of nothing is done.
            self.withdrawn(character, amount, TREASURY_SUCCESS);
            return;
        }
        let coins = CoinMove {
            character,
            company,
            amount,
        };
        self.change_treasury(Transfer::Withdraw(coins), coins, -change_of(amount));
    }

    /// The coins of transfer `ticket` moved (`Some` reply) or the sim did not answer (`None`).
    pub fn coins_moved(
        &mut self,
        ticket: u64,
        reply: Option<CoinReply>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let result = match reply {
            Some(CoinReply::Done { .. }) => MoveResult::Done,
            Some(CoinReply::NotHere | CoinReply::TooFew) => MoveResult::Refused,
            None => MoveResult::Unanswered,
        };
        if result == MoveResult::Unanswered {
            metrics::counter!(METRIC_UNANSWERED_MOVES).increment(1);
        }
        match (self.transfers.remove(&ticket), result) {
            (Some(Transfer::Deposit(coins)), MoveResult::Done) => {
                self.change_treasury(Transfer::Deposit(coins), coins, change_of(coins.amount));
            }
            (Some(Transfer::Deposit(coins)), MoveResult::Unanswered) => {
                tracing::error!(character = %coins.character, amount = coins.amount,
                    "deposit coin move not answered: Unknown whether the coins left; owed back");
                self.owe(coins);
                self.deposited(coins.character, NOTHING_DEPOSITED);
            }
            (Some(Transfer::Deposit(coins)), MoveResult::Refused) => {
                self.deposited(coins.character, NOTHING_DEPOSITED);
            }
            (Some(Transfer::Withdraw(coins)), MoveResult::Done) => {
                self.withdrawals
                    .entry(coins.character)
                    .or_default()
                    .push((now_ms, -change_of(coins.amount)));
                self.withdrawn(coins.character, coins.amount, TREASURY_SUCCESS);
            }
            (Some(Transfer::Withdraw(coins)), _) => {
                // The coins left the treasury but did not reach the player: they go back.
                self.change_treasury(Transfer::Refund(coins), coins, change_of(coins.amount));
                self.withdrawn(coins.character, coins.amount, TREASURY_TRANSFER_FAILED);
            }
            (
                Some(Transfer::DepositReturn(coins)),
                MoveResult::Refused | MoveResult::Unanswered,
            ) => {
                self.owe(coins);
            }
            (Some(Transfer::Debt(debt)), MoveResult::Done) => {
                self.store(StoreOp::RemoveCoinReturn(debt.id));
            }
            (Some(Transfer::Debt(debt)), MoveResult::Refused | MoveResult::Unanswered) => {
                self.owed.entry(debt.character).or_default().push(debt);
            }
            (Some(Transfer::DepositReturn(_) | Transfer::Refund(_)), _) | (None, _) => {}
        }
        self.refresh_views(now_ms);
        std::mem::take(&mut self.out)
    }

    /// What the database did with the treasury change of transfer `ticket`.
    pub fn treasury_changed(
        &mut self,
        ticket: u64,
        outcome: TreasuryOutcome,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(transfer) = self.transfers.remove(&ticket) else {
            return Vec::new();
        };
        let coins = match transfer {
            Transfer::Deposit(coins)
            | Transfer::Withdraw(coins)
            | Transfer::Refund(coins)
            | Transfer::DepositReturn(coins) => coins,
            Transfer::Debt(_) => return Vec::new(),
        };
        if let TreasuryOutcome::Stored(balance) = outcome
            && let Some(state) = self.companies.get_mut(&coins.company)
        {
            state.record.treasury = balance;
            let members = state.members.clone();
            for member in members {
                self.touch(member);
            }
        }
        let stored = matches!(outcome, TreasuryOutcome::Stored(_));
        match transfer {
            Transfer::Deposit(_) if stored => self.deposited(coins.character, coins.amount),
            Transfer::Deposit(_) => {
                // The coins left the player but the treasury did not take them: they go back.
                self.deposited(coins.character, NOTHING_DEPOSITED);
                let back = Transfer::DepositReturn(coins);
                if !self.move_coins(back, coins.character, change_of(coins.amount)) {
                    self.owe(coins);
                }
            }
            Transfer::Withdraw(_) if stored => {
                let to_player = Transfer::Withdraw(coins);
                if !self.move_coins(to_player, coins.character, change_of(coins.amount)) {
                    // The player left: the treasury takes the coins back.
                    self.change_treasury(Transfer::Refund(coins), coins, change_of(coins.amount));
                }
            }
            Transfer::Withdraw(_) => {
                let result = if outcome == TreasuryOutcome::Refused {
                    TREASURY_NOT_ENOUGH_TREASURY_FUNDS
                } else {
                    TREASURY_TRANSFER_FAILED
                };
                self.withdrawn(coins.character, coins.amount, result);
            }
            Transfer::Refund(_) if !stored => {
                // The treasury cannot take the coins back (its company is gone): the player gets
                // them.
                tracing::error!(company = %coins.company, amount = coins.amount,
                    "treasury refund refused; the coins go to the player");
                self.owe(coins);
            }
            Transfer::Refund(_) | Transfer::DepositReturn(_) | Transfer::Debt(_) => {}
        }
        self.refresh_views(now_ms);
        std::mem::take(&mut self.out)
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::social::guilds::{
        DEPOSIT_TYPE_INDEX, DEPOSITED_TYPE_INDEX, WITHDRAW_TYPE_INDEX, WITHDRAWN_TYPE_INDEX,
    };

    use super::*;
    use crate::companies::tests::{RULES, company_of_two};
    use crate::world::tests::{NOW_MS, call, character, join, pushes, sim};
    use nw_bus::SocialInput;

    const DEPOSIT: u64 = 500;
    const WITHDRAWAL: u64 = 100;

    fn deposit(world: &mut SocialWorld, number: u128, amount: u64) -> Vec<Effect> {
        call(
            world,
            number,
            "sim-a",
            DEPOSIT_TYPE_INDEX,
            &amount.to_be_bytes(),
            NOW_MS,
        )
    }

    fn withdraw(world: &mut SocialWorld, number: u128, amount: u64, now_ms: u64) -> Vec<Effect> {
        call(
            world,
            number,
            "sim-a",
            WITHDRAW_TYPE_INDEX,
            &amount.to_be_bytes(),
            now_ms,
        )
    }

    fn coin_move(effects: &[Effect]) -> (u64, i64) {
        let moves: Vec<(u64, i64)> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::MoveCoins {
                    ticket, transfer, ..
                } => Some((*ticket, transfer.change)),
                _ => None,
            })
            .collect();
        let [one] = moves.as_slice() else {
            panic!("one coin move: {effects:?}");
        };
        *one
    }

    fn treasury_change(effects: &[Effect]) -> (u64, i64) {
        let changes: Vec<(u64, i64)> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Treasury { ticket, change, .. } => Some((*ticket, *change)),
                _ => None,
            })
            .collect();
        let [one] = changes.as_slice() else {
            panic!("one treasury change: {effects:?}");
        };
        *one
    }

    fn debts(effects: &[Effect]) -> Vec<CoinReturn> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Store(StoreOp::AddCoinReturn(debt)) => Some(*debt),
                _ => None,
            })
            .collect()
    }

    fn withdrawn_answer(effects: &[Effect]) -> SocialPush {
        pushes(effects, WITHDRAWN_TYPE_INDEX)[0].1.clone()
    }

    fn deposited_answer(effects: &[Effect]) -> SocialPush {
        pushes(effects, DEPOSITED_TYPE_INDEX)[0].1.clone()
    }

    fn done() -> Option<CoinReply> {
        Some(CoinReply::Done { coins: 0 })
    }

    /// Ann's company with `amount` in its treasury.
    fn company_holding(amount: u64) -> (SocialWorld, Uuid) {
        let (mut world, company) = company_of_two();
        world
            .companies
            .get_mut(&company)
            .expect("company")
            .record
            .treasury = amount;
        (world, company)
    }

    #[test]
    fn a_deposit_and_a_withdrawal_move_coins_in_two_steps() {
        let (mut world, company) = company_of_two();
        let (ticket, change) = coin_move(&deposit(&mut world, 2, DEPOSIT));
        assert_eq!(change, -change_of(DEPOSIT));
        let (ticket, change) = treasury_change(&world.coins_moved(ticket, done(), NOW_MS));
        assert_eq!(change, change_of(DEPOSIT));
        let effects = world.treasury_changed(ticket, TreasuryOutcome::Stored(DEPOSIT), NOW_MS);
        assert_eq!(
            deposited_answer(&effects),
            SocialPush::Guild(GuildPush::Deposited { amount: DEPOSIT })
        );
        assert_eq!(world.companies[&company].record.treasury, DEPOSIT);
        assert_eq!(
            withdrawn_answer(&withdraw(&mut world, 2, WITHDRAWAL, NOW_MS)),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: WITHDRAWAL,
                result: TREASURY_NO_PRIVILEGE
            }),
            "a settler takes nothing"
        );
        let (ticket, change) = treasury_change(&withdraw(&mut world, 1, WITHDRAWAL, NOW_MS));
        assert_eq!(change, -change_of(WITHDRAWAL));
        let effects = world.treasury_changed(
            ticket,
            TreasuryOutcome::Stored(DEPOSIT - WITHDRAWAL),
            NOW_MS,
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::OwnState { .. })),
            "the members see the new treasury"
        );
        let (ticket, change) = coin_move(&effects);
        assert_eq!(change, change_of(WITHDRAWAL));
        let effects = world.coins_moved(ticket, done(), NOW_MS);
        assert_eq!(
            withdrawn_answer(&effects),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: WITHDRAWAL,
                result: TREASURY_SUCCESS
            })
        );
        assert_eq!(world.withdrawn_today(character(1), NOW_MS), WITHDRAWAL);
    }

    #[test]
    fn a_refused_deposit_is_answered_with_nothing_deposited() {
        let (mut world, _) = company_holding(RULES.treasury_max - WITHDRAWAL);
        let (ticket, change) = coin_move(&deposit(&mut world, 2, DEPOSIT));
        assert_eq!(change, -change_of(WITHDRAWAL), "only the room left");
        let effects = world.coins_moved(ticket, Some(CoinReply::TooFew), NOW_MS);
        assert_eq!(
            deposited_answer(&effects),
            SocialPush::Guild(GuildPush::Deposited {
                amount: NOTHING_DEPOSITED
            })
        );
        assert!(debts(&effects).is_empty(), "no coin left the player");
        let (mut world, _) = company_holding(RULES.treasury_max);
        let effects = deposit(&mut world, 2, DEPOSIT);
        assert_eq!(
            deposited_answer(&effects),
            SocialPush::Guild(GuildPush::Deposited {
                amount: NOTHING_DEPOSITED
            }),
            "a full treasury takes nothing and says so"
        );
    }

    #[test]
    fn deposit_coins_the_treasury_does_not_take_go_back_or_become_a_debt() {
        let (mut world, _) = company_of_two();
        let (ticket, _) = coin_move(&deposit(&mut world, 2, DEPOSIT));
        let (ticket, _) = treasury_change(&world.coins_moved(ticket, done(), NOW_MS));
        let effects = world.treasury_changed(ticket, TreasuryOutcome::Failed, NOW_MS);
        assert_eq!(
            deposited_answer(&effects),
            SocialPush::Guild(GuildPush::Deposited {
                amount: NOTHING_DEPOSITED
            })
        );
        let (ticket, change) = coin_move(&effects);
        assert_eq!(change, change_of(DEPOSIT), "the coins go back");
        let effects = world.coins_moved(ticket, Some(CoinReply::NotHere), NOW_MS);
        let [debt] = debts(&effects).try_into().expect("one debt");
        assert_eq!((debt.character, debt.amount), (character(2), DEPOSIT));
        // The player left before the treasury answered: the coins are a debt at once.
        let (ticket, _) = coin_move(&deposit(&mut world, 2, DEPOSIT));
        let (ticket, _) = treasury_change(&world.coins_moved(ticket, done(), NOW_MS));
        world.input(
            &sim("sim-a"),
            SocialInput::Left {
                character: character(2),
                reason: nw_bus::LeaveReason::Logout,
            },
            NOW_MS,
        );
        let effects = world.treasury_changed(ticket, TreasuryOutcome::Refused, NOW_MS);
        assert_eq!(debts(&effects).len(), 1);
        // Both debts are paid when the player is back, and end when its sim took them.
        let effects = join(&mut world, 2, "Bob", "sim-a");
        let moves: Vec<u64> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::MoveCoins {
                    ticket, transfer, ..
                } => {
                    assert_eq!(transfer.change, change_of(DEPOSIT));
                    Some(*ticket)
                }
                _ => None,
            })
            .collect();
        assert_eq!(moves.len(), 2);
        let effects = world.coins_moved(moves[0], done(), NOW_MS);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Store(StoreOp::RemoveCoinReturn(_))))
        );
        world.coins_moved(moves[1], None, NOW_MS);
        assert_eq!(world.owed[&character(2)].len(), 1, "an unpaid debt stays");
    }

    #[test]
    fn a_debt_is_paid_with_its_own_id_and_other_moves_get_new_ids() {
        let (mut world, _) = company_of_two();
        let move_id = |effects: &[Effect]| {
            effects
                .iter()
                .find_map(|effect| match effect {
                    Effect::MoveCoins { transfer, .. } => Some(transfer.id),
                    _ => None,
                })
                .expect("a coin move")
        };
        let first = move_id(&deposit(&mut world, 2, DEPOSIT));
        let second = move_id(&deposit(&mut world, 2, DEPOSIT));
        assert_ne!(first, second, "each deposit is its own move");
        let debt = CoinReturn {
            id: Uuid::from_u128(77),
            character: character(2),
            company: None,
            amount: DEPOSIT,
        };
        world.owed.insert(character(2), vec![debt]);
        world.pay_debts(character(2));
        let effects = std::mem::take(&mut world.out);
        assert_eq!(
            move_id(&effects),
            debt.id,
            "a repeat of the debt moves once"
        );
    }

    #[test]
    fn an_unanswered_deposit_move_is_owed_back() {
        let (mut world, _) = company_of_two();
        let (ticket, _) = coin_move(&deposit(&mut world, 2, DEPOSIT));
        let effects = world.coins_moved(ticket, None, NOW_MS);
        assert_eq!(debts(&effects).len(), 1);
        assert_eq!(
            deposited_answer(&effects),
            SocialPush::Guild(GuildPush::Deposited {
                amount: NOTHING_DEPOSITED
            })
        );
    }

    #[test]
    fn a_withdrawal_that_does_not_reach_the_player_goes_back_and_is_not_counted() {
        let (mut world, company) = company_holding(DEPOSIT);
        world
            .companies
            .get_mut(&company)
            .expect("company")
            .record
            .withdrawal_limit = Some(WITHDRAWAL);
        world.people.get_mut(&character(2)).expect("Bob").company = Some((company, 2));
        let (ticket, _) = treasury_change(&withdraw(&mut world, 2, WITHDRAWAL, NOW_MS));
        assert_eq!(
            withdrawn_answer(&withdraw(&mut world, 2, WITHDRAWAL, NOW_MS)),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: WITHDRAWAL,
                result: TREASURY_DAILY_LIMIT
            }),
            "a withdrawal on its way counts"
        );
        let effects = world.treasury_changed(
            ticket,
            TreasuryOutcome::Stored(DEPOSIT - WITHDRAWAL),
            NOW_MS,
        );
        let (ticket, _) = coin_move(&effects);
        let effects = world.coins_moved(ticket, Some(CoinReply::NotHere), NOW_MS);
        assert_eq!(
            withdrawn_answer(&effects),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: WITHDRAWAL,
                result: TREASURY_TRANSFER_FAILED
            })
        );
        let (ticket, change) = treasury_change(&effects);
        assert_eq!(change, change_of(WITHDRAWAL), "the treasury takes it back");
        assert_eq!(world.withdrawn_today(character(2), NOW_MS), 0);
        // The company is gone: the refund becomes a debt to the player.
        let effects = world.treasury_changed(ticket, TreasuryOutcome::Refused, NOW_MS);
        assert_eq!(debts(&effects)[0].amount, WITHDRAWAL);
    }

    #[test]
    fn a_failed_withdrawal_names_its_reason() {
        let (mut world, _) = company_holding(DEPOSIT);
        assert_eq!(
            withdrawn_answer(&withdraw(&mut world, 1, DEPOSIT + 1, NOW_MS)),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: DEPOSIT + 1,
                result: TREASURY_NOT_ENOUGH_TREASURY_FUNDS
            })
        );
        let (ticket, _) = treasury_change(&withdraw(&mut world, 1, WITHDRAWAL, NOW_MS));
        assert_eq!(
            withdrawn_answer(&world.treasury_changed(ticket, TreasuryOutcome::Failed, NOW_MS)),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: WITHDRAWAL,
                result: TREASURY_TRANSFER_FAILED
            }),
            "a database failure is not 'not enough funds'"
        );
        join(&mut world, 3, "Cid", "sim-a");
        assert_eq!(
            withdrawn_answer(&withdraw(&mut world, 3, WITHDRAWAL, NOW_MS)),
            SocialPush::Guild(GuildPush::Withdrawn {
                amount: WITHDRAWAL,
                result: TREASURY_NOT_A_MEMBER
            })
        );
    }
}
