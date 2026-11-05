// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors from database records.

/// Failure while reading or writing a record.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// That username is already registered.
    #[error("username is already registered")]
    DuplicateUsername,
    /// The account already has a Discord login of another Discord user.
    #[error("account already has a Discord login")]
    AccountHasDiscordLogin,
    /// That character name is already used in some world.
    #[error("character name is already used")]
    DuplicateCharacterName,
    /// The world database has no character with that id.
    #[error("no such character")]
    NoSuchCharacter,
    /// The database schema is not at the version this program needs.
    #[error(
        "database schema is at migration {found}, this program needs {expected}; run `edge-http migrate`"
    )]
    SchemaNotCurrent {
        /// Newest migration of this program.
        expected: i64,
        /// Newest applied migration, 0 when none.
        found: i64,
    },
    /// A migration failed.
    #[error("migration failed")]
    Migrate(#[source] Box<sqlx::migrate::MigrateError>),
    /// A value does not fit its column (coins above the BIGINT range, an achievement bit above
    /// the INTEGER range), or a stored value does not fit the program's type.
    #[error("{field} is out of range")]
    OutOfRange {
        /// The column.
        field: &'static str,
    },
    /// PostgreSQL returned an error.
    #[error("database error")]
    Database(#[source] Box<sqlx::Error>),
}

/// SQLSTATE classes and codes of a failure that may pass when the same write runs again: class
/// 08 (connection exception), 53 (insufficient resources), 40001 (serialization failure), 40P01
/// (deadlock), 57P01 (admin shutdown), 57P02 (crash shutdown), 57P03 (cannot connect now). From
/// the PostgreSQL error code table.
const TRANSIENT_STATE_CLASSES: [&str; 2] = ["08", "53"];
const TRANSIENT_STATES: [&str; 5] = ["40001", "40P01", "57P01", "57P02", "57P03"];
/// Characters of a SQLSTATE class.
const STATE_CLASS_LEN: usize = 2;

impl DbError {
    /// True when the same write may pass if it runs again: the database could not be reached or
    /// was busy. False when PostgreSQL refused the write itself (a constraint, a type, a range),
    /// which a retry repeats.
    pub fn is_transient(&self) -> bool {
        let Self::Database(error) = self else {
            return false;
        };
        match error.as_ref() {
            sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed => true,
            sqlx::Error::Database(database) => database.code().is_some_and(|code| {
                TRANSIENT_STATES.contains(&code.as_ref())
                    || code
                        .get(..STATE_CLASS_LEN)
                        .is_some_and(|class| TRANSIENT_STATE_CLASSES.contains(&class))
            }),
            _ => false,
        }
    }
}

impl From<sqlx::Error> for DbError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(Box::new(error))
    }
}

impl From<sqlx::migrate::MigrateError> for DbError {
    fn from(error: sqlx::migrate::MigrateError) -> Self {
        Self::Migrate(Box::new(error))
    }
}

/// True when `error` is a unique violation of the named constraint.
pub(crate) fn is_unique_violation(error: &sqlx::Error, constraint: &str) -> bool {
    error.as_database_error().is_some_and(|database| {
        database.is_unique_violation() && database.constraint() == Some(constraint)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_connection_is_transient_and_a_refused_value_is_not() {
        assert!(DbError::from(sqlx::Error::PoolTimedOut).is_transient());
        assert!(
            DbError::from(sqlx::Error::Io(std::io::Error::from(
                std::io::ErrorKind::ConnectionReset
            )))
            .is_transient()
        );
        assert!(!DbError::from(sqlx::Error::RowNotFound).is_transient());
        assert!(!DbError::OutOfRange { field: "amount" }.is_transient());
        assert!(!DbError::NoSuchCharacter.is_transient());
    }
}
