//! Device-global commands and their execution against the application backend.
//!
//! One enum carries the command on the wire and one match runs it. Settings
//! writes are the only commands that must observe receive order; they are
//! listed by [`AppCommand::is_settings_update`] and executed by
//! [`execute_ordered`], which both transports reach through their ordered path.
use super::dispatch::storage_command;
use super::*;
use client_runtime::reply::Reply;
fn reply<T: crate::executor::BackendOutput + serde::Serialize>(value: T) -> Result<Reply, crate::BackendError> {
    Ok(Reply::new(value))
}

pub(crate) type RequestFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, crate::BackendError>>>>;

fn message(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> crate::BackendError {
    crate::BackendError::operation(error)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum AppCommand {
    Libraries,
    ReconcileLibraries,
    DefaultLibrarySaveLocation,
    SetDefaultLibrarySaveLocation {
        path: String,
    },
    UpdateBrowsingPreference {
        patch: app_preferences::BrowsingPreferencePatch,
    },
    UpdateReaderPreferences {
        patches: Vec<app_preferences::ReaderPreferencePatch>,
    },
    EstimateLibraryFolders {
        locators: Vec<String>,
    },
    CreateLibraryWithAssetStorage {
        name: String,
        asset_storage_enabled: bool,
    },
    CreateLibrariesWithAssetStorage {
        locators: Vec<String>,
        asset_storage_enabled: bool,
    },
    CreateLibrary {
        name: String,
    },
    SetLibraryAssetStorage {
        library_id: LibraryId,
        enabled: bool,
    },
    EnsureImportLibrary,
    CreateLibrariesFromPaths {
        locators: Vec<String>,
    },
    RenameLibrary {
        library_id: LibraryId,
        name: String,
    },
    DeleteLibrary {
        library_id: LibraryId,
    },
    RequestPublicRegistration {
        email: String,
        password: String,
    },
    ResendVerification {
        email: String,
    },
    RequestPasswordReset {
        email: String,
    },
    ResetPassword {
        token: String,
        password: String,
    },
    AccountSnapshot,
    LocalStorageUsage,
    Transfers,
    Login {
        email: String,
        password: String,
    },
    VerifyEmail {
        email: String,
        pin: String,
    },
    Logout,
    SubscribeNotifications {
        browsing: bool,
    },
    SubscribeLibraryList,
    #[serde(untagged)]
    Host(super::HostAppCommand),
}

impl AppCommand {
    /// Settings writes run in receive order so two patches issued together
    /// cannot land in the opposite order. Everything else runs concurrently.
    pub(crate) fn is_settings_update(&self) -> bool {
        matches!(self, Self::DefaultLibrarySaveLocation | Self::SetDefaultLibrarySaveLocation { .. } | Self::UpdateBrowsingPreference { .. } | Self::UpdateReaderPreferences { .. })
    }

    pub(crate) fn is_book_subscription(&self) -> bool {
        match self {
            Self::Host(command) => command.is_book_subscription(),
            _ => false,
        }
    }
}

/// The body of every ordered command. Native dispatch runs it directly in
/// receive order; the wire dispatcher queues the same call for storage.
pub(crate) fn execute_ordered(backend: &AppBackend, command: AppCommand) -> Result<Reply, crate::BackendError> {
    match command {
        AppCommand::DefaultLibrarySaveLocation => reply(backend.default_library_save_location().map_err(message)?),
        AppCommand::SetDefaultLibrarySaveLocation { path } => reply(backend.set_default_library_save_location(path).map_err(message)?),
        AppCommand::UpdateBrowsingPreference { patch } => reply(backend.update_browsing_preference(patch).map_err(message)?),
        AppCommand::UpdateReaderPreferences { patches } => {
            if !patches.is_empty() {
                backend.update_reader_preferences(patches).map_err(message)?;
            }
            reply(())
        }
        command => Err(crate::BackendError::message(format!("{command:?} does not support ordered execution"))),
    }
}

pub(super) fn dispatch_request(backend: AppBackend, command: AppCommand) -> RequestFuture<WorkerDispatchResult<Reply>> {
    Box::pin(async move {
        if command.is_settings_update() {
            return storage_command(move || execute_ordered(&backend, command)).await.map(WorkerDispatchResult::Response);
        }
        let reply = match command {
            AppCommand::Libraries => storage_command(move || reply(backend.libraries().map_err(message)?)).await?,
            AppCommand::ReconcileLibraries => reply(backend.reconcile_libraries().await.map_err(message)?)?,

            AppCommand::EstimateLibraryFolders { locators } => crate::executor::run_blocking(move || {
                let cancelled = std::sync::atomic::AtomicBool::new(false);
                reply(crate::library::discovery::estimate_folders(locators, &cancelled).map_err(message)?)
            })
            .await
            .map_err(message)??,

            AppCommand::CreateLibraryWithAssetStorage { name, asset_storage_enabled } => storage_command(move || reply(backend.create_library_with_asset_storage(name, asset_storage_enabled).map_err(message)?)).await?,
            AppCommand::CreateLibrariesWithAssetStorage { locators, asset_storage_enabled } => {
                storage_command(move || reply(backend.create_libraries_from_paths_with_asset_storage(locators.into_iter().map(std::path::PathBuf::from).collect(), asset_storage_enabled).map_err(message)?)).await?
            }
            AppCommand::CreateLibrary { name } => storage_command(move || reply(backend.create_library(name, None).map_err(message)?)).await?,
            AppCommand::SetLibraryAssetStorage { library_id, enabled } => storage_command(move || reply(backend.set_library_asset_storage(library_id, enabled).map_err(message)?)).await?,
            AppCommand::EnsureImportLibrary => storage_command(move || reply(backend.ensure_import_library().map_err(message)?)).await?,
            AppCommand::CreateLibrariesFromPaths { locators } => storage_command(move || reply(backend.create_libraries_from_paths(locators.into_iter().map(std::path::PathBuf::from).collect()).map_err(message)?)).await?,
            AppCommand::RenameLibrary { library_id, name } => storage_command(move || reply(backend.rename_library(&library_id, &name).map_err(message)?)).await?,
            AppCommand::DeleteLibrary { library_id } => reply(backend.delete_library(&library_id).await.map_err(message)?)?,

            AppCommand::RequestPublicRegistration { email, password } => reply(account_client::request_public_registration(&crate::api_server_url(), &email, &password).await.map_err(message)?)?,
            AppCommand::ResendVerification { email } => reply(account_client::resend_verification(&crate::api_server_url(), &email).await.map_err(message)?)?,
            AppCommand::RequestPasswordReset { email } => reply(account_client::request_password_reset(&crate::api_server_url(), &email).await.map_err(message)?)?,
            AppCommand::ResetPassword { token, password } => reply(account_client::reset_password(&crate::api_server_url(), &token, &password).await.map_err(message)?)?,

            AppCommand::AccountSnapshot => reply(backend.account_snapshot().await.map_err(message)?)?,
            AppCommand::LocalStorageUsage => reply(backend.local_storage_usage().await.map_err(message)?)?,
            AppCommand::Transfers => reply(backend.library_directory().transfer_snapshots().await.map_err(message)?)?,
            AppCommand::Login { email, password } => {
                backend.login(&email, &password).await.map_err(message)?;
                reply(backend.account_snapshot().await.map_err(message)?)?
            }
            AppCommand::VerifyEmail { email, pin } => {
                backend.verify_email(&email, &pin).await.map_err(message)?;
                reply(backend.account_snapshot().await.map_err(message)?)?
            }
            AppCommand::Logout => {
                backend.logout().map_err(message)?;
                reply(backend.account_snapshot().await.map_err(message)?)?
            }

            AppCommand::SubscribeNotifications { browsing } => {
                return Ok(WorkerDispatchResult::Subscription { initial: reply(())?, events: WorkerSubscription::Unit(backend.notification_interest(browsing)) });
            }
            AppCommand::SubscribeLibraryList => {
                return Ok(WorkerDispatchResult::Subscription { initial: reply(())?, events: WorkerSubscription::Unit(backend.library_list_changes()) });
            }

            AppCommand::Host(command) => return command.dispatch_app(backend).await,

            // Routed above; listed so a new settings command cannot be missed.
            AppCommand::DefaultLibrarySaveLocation | AppCommand::SetDefaultLibrarySaveLocation { .. } | AppCommand::UpdateBrowsingPreference { .. } | AppCommand::UpdateReaderPreferences { .. } => {
                unreachable!("settings commands run through execute_ordered")
            }
        };
        Ok(WorkerDispatchResult::Response(reply))
    })
}
