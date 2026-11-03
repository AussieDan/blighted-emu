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
