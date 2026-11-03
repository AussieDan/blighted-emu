// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP errors of the launcher API and the game routes (M2 spec section 10): one JSON body
//! `{"code": "...", "message": "..."}` and a status code. On the game routes the client reads only
//! the status (follow-up note section 4).

use axum::Json;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nw_protocol::launcher_api::ErrorBody;
use serde::de::DeserializeOwned;

use crate::names::NameError;
use crate::store::StoreError;

/// An error that a route returns to the caller.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The body is not the expected JSON.
    #[error("request body is not valid")]
    BadRequest,
    /// A name or password breaks a rule.
    #[error(transparent)]
    InvalidName(#[from] NameError),
    /// Unknown user or wrong password.
    #[error("username or password is wrong")]
    InvalidCredentials,
    /// No valid launcher session.
    #[error("log in again")]
    NoSession,
    /// The account is banned.
    #[error("this account is banned")]
    Banned,
    /// Password sign-up is closed on this server.
    #[error("sign-up is closed on this server")]
    RegistrationClosed,
    /// The route belongs to the other login mode.
    #[error("this server does not use this login mode")]
    WrongMode,
    /// The username is taken.
    #[error("username is already registered")]
    UsernameTaken,
    /// The character name is taken.
    #[error("that character name is taken")]
    CharacterNameTaken,
    /// The account has the most characters a world allows.
    #[error("you have the most characters this world allows")]
    CharacterLimit,
    /// No world with that id.
    #[error("unknown world")]
    UnknownWorld,
    /// Too many attempts.
    #[error("too many attempts, try again later")]
    RateLimited,
    /// The Discord login attempt is unknown or expired.
    #[error("Discord login expired, start again")]
    UnknownLogin,
    /// Discord refused or failed.
    #[error("Discord login failed")]
    Discord,
    /// The Discord user is not in the Discord server that logins need.
    #[error("join the server's Discord first")]
    NotGuildMember,
    /// No character of this account with that id in that world.
    #[error("unknown character")]
    UnknownCharacter,
    /// No queue entry of this account with that ticket id.
    #[error("unknown queue ticket")]
    UnknownTicket,
    /// The request names a host this server does not serve.
    #[error("unknown host")]
    UnknownHost,
    /// No route serves this request.
    #[error("unknown route")]
    UnknownRoute,
    /// A store or worker failed.
    #[error("service unavailable")]
    Unavailable,
}

impl ApiError {
    /// HTTP status of the error.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest | Self::InvalidName(_) => StatusCode::BAD_REQUEST,
            Self::InvalidCredentials | Self::NoSession => StatusCode::UNAUTHORIZED,
            Self::Banned | Self::RegistrationClosed | Self::NotGuildMember => StatusCode::FORBIDDEN,
            Self::WrongMode
            | Self::UnknownLogin
            | Self::UnknownWorld
            | Self::UnknownCharacter
            | Self::UnknownTicket
            | Self::UnknownRoute => StatusCode::NOT_FOUND,
            Self::UsernameTaken | Self::CharacterNameTaken | Self::CharacterLimit => {
                StatusCode::CONFLICT
            }
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::UnknownHost => StatusCode::MISDIRECTED_REQUEST,
            Self::Discord => StatusCode::BAD_GATEWAY,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// Stable snake_case code of the error.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::InvalidName(NameError::Username) => "invalid_username",
            Self::InvalidName(NameError::Password) => "invalid_password",
            Self::InvalidName(NameError::CharacterName) => "invalid_character_name",
            Self::InvalidCredentials => "invalid_credentials",
            Self::NoSession => "no_session",
            Self::Banned => "banned",
            Self::RegistrationClosed => "registration_closed",
            Self::WrongMode => "wrong_mode",
            Self::UsernameTaken => "username_taken",
            Self::CharacterNameTaken => "character_name_taken",
            Self::CharacterLimit => "character_limit",
            Self::UnknownWorld => "unknown_world",
            Self::UnknownCharacter => "unknown_character",
            Self::UnknownTicket => "unknown_ticket",
            Self::UnknownHost => "unknown_host",
            Self::UnknownRoute => "unknown_route",
            Self::RateLimited => "rate_limited",
            Self::UnknownLogin => "unknown_login",
            Self::Discord => "discord_failed",
            Self::NotGuildMember => "not_guild_member",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Parses a JSON request body; a body that does not parse is [`ApiError::BadRequest`].
pub(crate) fn parse_json<T: DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|_| ApiError::BadRequest)
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::DuplicateUsername => Self::UsernameTaken,
            StoreError::DuplicateCharacterName => Self::CharacterNameTaken,
            StoreError::UnknownWorld => Self::UnknownWorld,
            StoreError::Unavailable(source) => {
                tracing::error!(error = %source, "store failed");
                Self::Unavailable
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code().to_owned(),
            message: self.to_string(),
        };
        (self.status(), Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_a_status_and_code() {
        let errors = [
            ApiError::BadRequest,
            ApiError::InvalidName(NameError::Password),
            ApiError::InvalidCredentials,
            ApiError::NoSession,
            ApiError::Banned,
            ApiError::RegistrationClosed,
            ApiError::WrongMode,
            ApiError::UsernameTaken,
            ApiError::CharacterNameTaken,
            ApiError::CharacterLimit,
            ApiError::UnknownWorld,
            ApiError::UnknownCharacter,
            ApiError::UnknownTicket,
            ApiError::UnknownHost,
            ApiError::UnknownRoute,
            ApiError::RateLimited,
            ApiError::UnknownLogin,
            ApiError::Discord,
            ApiError::NotGuildMember,
            ApiError::Unavailable,
        ];
        let codes: std::collections::HashSet<&str> = errors.iter().map(ApiError::code).collect();
        assert_eq!(codes.len(), errors.len());
        assert_eq!(
            ApiError::RateLimited.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ApiError::from(StoreError::DuplicateUsername).code(),
            "username_taken"
        );
        assert_eq!(
            ApiError::from(StoreError::DuplicateCharacterName).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ApiError::from(StoreError::UnknownWorld).status(),
            StatusCode::NOT_FOUND
        );
    }
}
