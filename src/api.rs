// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Command output data layer.
//!
//! Each command builds one of these report types from its operation
//! results (never from echoed args) and renders it through [`Printable`]:
//! `print_plain` makes exactly one `print_table` call with static
//! headers; `print_json` emits a pretty JSON array (or, for `init`, a
//! single object) with no wrapper.

pub mod add_report;
pub mod artifact_status;
pub mod build_report;
pub mod config_report;
pub mod init_report;
pub mod install_report;
pub mod lock_report;
pub mod login_report;
pub mod publish_report;
pub mod release_report;
pub mod remove_report;
pub mod search_report;
pub mod status_report;
pub mod uninstall_report;
pub mod update_report;

#[allow(unused_imports)]
pub use add_report::{AddReport, AddStatus};
#[allow(unused_imports)]
pub use artifact_status::{ArtifactStatus, InitStatus, InstallStatus, LockAction, UpdateAction};
#[allow(unused_imports)]
pub use build_report::{BuildReport, BuildStatus};
#[allow(unused_imports)]
pub use config_report::{
    ConfigEntry, ConfigGetReport, ConfigListReport, ConfigReport, ConfigWriteReport, Origin, RegistryListReport,
    RegistryRow, RegistryShowReport, WriteAction,
};
#[allow(unused_imports)]
pub use init_report::InitReport;
#[allow(unused_imports)]
pub use install_report::{InstallEntry, InstallReport};
#[allow(unused_imports)]
pub use lock_report::{LockEntry, LockReport};
#[allow(unused_imports)]
pub use login_report::{LoginReport, LogoutReport};
#[allow(unused_imports)]
pub use publish_report::{PublishEntry, PublishReport, PublishStatus};
#[allow(unused_imports)]
pub use release_report::ReleaseReport;
#[allow(unused_imports)]
pub use remove_report::{RemoveReport, RemoveStatus};
#[allow(unused_imports)]
pub use search_report::{SearchEntry, SearchReport};
#[allow(unused_imports)]
pub use status_report::{StatusEntry, StatusReport};
#[allow(unused_imports)]
pub use update_report::{UpdateEntry, UpdateReport};
