//! Generic registry trait and error types.

use uuid::Uuid;

use crate::aria::types::Pagination;

/// Error type for all registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Not found: {entity} with id {id}")]
    NotFound { entity: String, id: String },

    #[error("Duplicate: {entity} '{name}' already exists for tenant {tenant_id}")]
    Duplicate {
        entity: String,
        name: String,
        tenant_id: String,
    },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<tokio_postgres::Error> for RegistryError {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<deadpool_postgres::PoolError> for RegistryError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<serde_json::Error> for RegistryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

pub type RegistryResult<T> = std::result::Result<T, RegistryError>;

/// Trait for registry operations common to most registries.
///
/// Individual registries implement domain-specific methods beyond this trait
/// (e.g., `updateStatus`, `sweepExpired`, `listByNetwork`).
#[async_trait::async_trait]
pub trait Registry: Send + Sync {
    type Entry: Send + Sync;
    type Upload: Send + Sync;

    /// Upload (upsert) an entry. Same tenant+name = update.
    async fn upload(&self, tenant_id: &str, input: Self::Upload) -> RegistryResult<Self::Entry>;

    /// Get by ID.
    async fn get(&self, id: Uuid) -> RegistryResult<Self::Entry>;

    /// Get by tenant + name.
    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<Self::Entry>;

    /// List entries for a tenant.
    async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<Self::Entry>>;

    /// Count entries for a tenant.
    async fn count(&self, tenant_id: &str) -> RegistryResult<i64>;

    /// Soft-delete by ID.
    async fn delete(&self, id: Uuid) -> RegistryResult<()>;
}
