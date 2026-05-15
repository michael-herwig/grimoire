// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Process exit codes for the `grim` binary.
//!
//! Numeric values align with BSD `sysexits.h` (EX__BASE = 64) to avoid
//! collisions with shell-reserved codes (1–2) and signal-derived codes
//! (128+). Scripts can `case $?` on these values for structured error
//! handling.

/// Process exit codes used by the `grim` binary.
///
/// Numeric values align with BSD `sysexits.h` (EX__BASE = 64) to avoid
/// collisions with shell-reserved codes (1–2) and signal-derived codes
/// (128+). Scripts can `case $?` on these values for structured error
/// handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    /// Successful completion.
    Success = 0,
    /// Generic failure — use only when no specific code applies.
    Failure = 1,
    /// Bad CLI invocation: unknown flag, wrong argument count, invalid syntax.
    /// Mirrors `EX_USAGE` (64).
    UsageError = 64,
    /// Input data malformed: bad identifier format, invalid digest.
    /// Mirrors `EX_DATAERR` (65).
    DataError = 65,
    /// Required resource unavailable: network down, registry unreachable.
    /// Mirrors `EX_UNAVAILABLE` (69).
    Unavailable = 69,
    /// I/O error: filesystem permission denied, disk full, read/write failure.
    /// Mirrors `EX_IOERR` (74).
    IoError = 74,
    /// Temporary failure that may succeed on retry: rate limit, transient network.
    /// Mirrors `EX_TEMPFAIL` (75).
    TempFail = 75,
    /// Insufficient permissions: registry 403, filesystem `EPERM`.
    /// Mirrors `EX_NOPERM` (77).
    NoPermission = 77,
    /// Configuration error: bad config file, missing required field, parse failure.
    /// Mirrors `EX_CONFIG` (78).
    ConfigError = 78,
    /// Resource not found: package 404, explicit config path absent.
    /// Grimoire-specific; first slot above `EX_CONFIG`.
    NotFound = 79,
    /// Authentication failure: registry 401, missing credentials.
    /// Grimoire-specific.
    AuthError = 80,
    /// Offline mode blocked a network operation.
    /// Distinct from `Unavailable`: the failure is deliberate policy, not a fault.
    OfflineBlocked = 81,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(value: ExitCode) -> Self {
        std::process::ExitCode::from(value as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each assertion quotes the canonical numeric value from
    // quality-rust-exit_codes.md, NOT derived from the enum definition.
    // If the enum value ever drifts, the test catches it.

    #[test]
    fn exit_code_success_is_zero() {
        assert_eq!(ExitCode::Success as u8, 0);
    }

    #[test]
    fn exit_code_failure_is_one() {
        assert_eq!(ExitCode::Failure as u8, 1);
    }

    #[test]
    fn exit_code_usage_error_is_64() {
        assert_eq!(ExitCode::UsageError as u8, 64);
    }

    #[test]
    fn exit_code_data_error_is_65() {
        assert_eq!(ExitCode::DataError as u8, 65);
    }

    #[test]
    fn exit_code_unavailable_is_69() {
        assert_eq!(ExitCode::Unavailable as u8, 69);
    }

    #[test]
    fn exit_code_io_error_is_74() {
        assert_eq!(ExitCode::IoError as u8, 74);
    }

    #[test]
    fn exit_code_temp_fail_is_75() {
        assert_eq!(ExitCode::TempFail as u8, 75);
    }

    #[test]
    fn exit_code_no_permission_is_77() {
        assert_eq!(ExitCode::NoPermission as u8, 77);
    }

    #[test]
    fn exit_code_config_error_is_78() {
        assert_eq!(ExitCode::ConfigError as u8, 78);
    }

    #[test]
    fn exit_code_not_found_is_79() {
        assert_eq!(ExitCode::NotFound as u8, 79);
    }

    #[test]
    fn exit_code_auth_error_is_80() {
        assert_eq!(ExitCode::AuthError as u8, 80);
    }

    #[test]
    fn exit_code_offline_blocked_is_81() {
        assert_eq!(ExitCode::OfflineBlocked as u8, 81);
    }

    #[test]
    fn exit_code_converts_to_process_exit_code() {
        let _: std::process::ExitCode = ExitCode::Success.into();
        let _: std::process::ExitCode = ExitCode::Failure.into();
        let _: std::process::ExitCode = ExitCode::ConfigError.into();
    }
}
