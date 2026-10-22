//! Every bound in one place. Untrusted input is checked against these before use.

pub const NAME_MAX: usize = 120;
pub const FIELD_NAME_MAX: usize = 80;
pub const SELECTOR_MAX: usize = 500;
pub const URL_MAX: usize = 2048;
pub const ID_MAX: usize = 64;

pub const FIELDS_MAX: usize = 30;
pub const ACTIONS_MAX: usize = 20;
pub const ACTION_WAIT_MAX_MS: u32 = 5_000;
pub const STEPS_MAX: usize = 50;
pub const STEP_WAIT_MAX_MS: u32 = 30_000;
pub const REPEAT_MAX: u32 = 30;
pub const PAGES_MAX: u32 = 30;
/// Schedules are off (0) or between 15 minutes and 30 days.
pub const SCHEDULE_MIN_MINUTES: u32 = 15;
pub const SCHEDULE_MAX_MINUTES: u32 = 43_200;

/// Rows and bytes one run may collect.
pub const RUN_ROWS_MAX: usize = 5_000;
pub const RUN_BYTES_MAX: usize = 2_000_000;
/// One extracted value; larger values mean the selector is too broad.
pub const VALUE_BYTES_MAX: usize = 100_000;
/// One record (all its values and its page address). Keeps every sealed record well
/// under the native messaging limit after base64.
pub const ROW_BYTES_MAX: usize = 512 * 1024;
/// Warnings one extraction may return, and one run may keep.
pub const EXTRACT_WARNINGS_MAX: usize = 30;
pub const RUN_WARNINGS_MAX: usize = 100;
pub const WARNING_BYTES_MAX: usize = 1_000;

/// Primitive timeouts the drivers apply, and the planner's waits.
pub const ACTION_TIMEOUT_MS: u32 = 20_000;
pub const SETTLE_MS: u32 = 400;
pub const POLL_MS: u32 = 250;
pub const PAGE_CHANGE_TIMEOUT_MS: u64 = 20_000;
pub const RUN_TIMEOUT_DEFAULT_MS: u64 = 180_000;
pub const RUN_TIMEOUT_MAX_MS: u64 = 2_147_483_647;

/// Native messaging: Chrome refuses host messages over 1 MiB.
pub const HOST_MESSAGE_MAX: usize = 1024 * 1024;
/// Messages from the extension to the host (Chrome allows 64 MiB); bridge replies can
/// carry many records.
pub const HOST_INBOUND_MAX: usize = 16 * 1024 * 1024;

pub const PASSPHRASE_MIN_CHARS: usize = 12;
pub const PASSPHRASE_MAX_BYTES: usize = 1024;
