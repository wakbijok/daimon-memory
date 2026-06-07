//! Embedding + Qdrant vector store for daimon-memory semantic recall.
//!
//! - [`Embedder`]: in-process fastembed **bge-small-en-v1.5** (384-d, CPU) — no external
//!   service, no big model (SDS v0.2 Principle 1). `embed` needs `&mut`, so the model is
//!   held behind a `Mutex`.
//! - [`VectorStore`]: one Qdrant collection (`daimon_memory`) with `tenant_id` in the
//!   payload; search is tenant-filtered. Point id = the record UUID (idempotent upsert).
//!
//! API patterns mirror the proven monorepo `daimon-memory::vector` (qdrant-client 1.18).

use daimon_memory_core::{MemoryError, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, DeletePointsBuilder, Distance, Filter, PointStruct,
    PointsIdsList, SearchPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
};
use serde_json::Value as Json;
use std::sync::Mutex;
use uuid::Uuid;

/// Embedding dimension of bge-small-en-v1.5.
pub const DIM: u64 = 384;
/// Single shared collection; tenant isolation via payload filter.
pub const COLLECTION: &str = "daimon_memory";

fn qe<E: std::fmt::Display>(e: E) -> MemoryError {
    MemoryError::Backend(e.to_string())
}

/// In-process dense embedder (bge-small, 384-d).
pub struct Embedder {
    inner: Mutex<TextEmbedding>,
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let opts = TextInitOptions::new(EmbeddingModel::BGESmallENV15);
        let inner = TextEmbedding::try_new(opts)
            .map_err(|e| MemoryError::Backend(format!("embedder init: {e}")))?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| MemoryError::Backend("embedder mutex poisoned".into()))?;
        g.embed(texts.to_vec(), None)
            .map_err(|e| MemoryError::Backend(format!("embed: {e}")))
    }

    pub fn dim(&self) -> u64 {
        DIM
    }
}

/// A scored vector hit (payload carries `uri`, `kind`, `title`, `abstract`, …).
#[derive(Debug, Clone)]
pub struct VecHit {
    pub score: f32,
    pub payload: Json,
}

/// Qdrant-backed vector store.
pub struct VectorStore {
    client: Qdrant,
}

impl VectorStore {
    /// Connect to a Qdrant gRPC endpoint (e.g. `http://host:6334`).
    pub fn connect(url: &str) -> Result<Self> {
        let client = Qdrant::from_url(url)
            .build()
            .map_err(|e| MemoryError::Backend(format!("qdrant connect: {e}")))?;
        Ok(Self { client })
    }

    /// Create the collection if absent (idempotent).
    pub async fn ensure(&self) -> Result<()> {
        if self.client.collection_exists(COLLECTION).await.map_err(qe)? {
            return Ok(());
        }
        let req = CreateCollectionBuilder::new(COLLECTION)
            .vectors_config(VectorParamsBuilder::new(DIM, Distance::Cosine));
        self.client.create_collection(req).await.map_err(qe)?;
        Ok(())
    }

    /// Upsert one record's vector + payload (point id = record UUID).
    pub async fn upsert(&self, id: Uuid, vector: Vec<f32>, payload: Json) -> Result<()> {
        let point = PointStruct::new(
            id.to_string(),
            vector,
            qdrant_client::Payload::try_from(payload).unwrap_or_default(),
        );
        let req = UpsertPointsBuilder::new(COLLECTION, vec![point]).wait(true);
        self.client.upsert_points(req).await.map_err(qe)?;
        Ok(())
    }

    /// Delete a record's point.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let req = DeletePointsBuilder::new(COLLECTION)
            .points(PointsIdsList {
                ids: vec![id.to_string().into()],
            })
            .wait(true);
        self.client.delete_points(req).await.map_err(qe)?;
        Ok(())
    }

    /// Tenant-filtered nearest-neighbour search.
    pub async fn search(&self, tenant: Uuid, query: Vec<f32>, top_k: u64) -> Result<Vec<VecHit>> {
        let filter = Filter::must([Condition::matches("tenant_id", tenant.to_string())]);
        let req = SearchPointsBuilder::new(COLLECTION, query, top_k)
            .filter(filter)
            .with_payload(true);
        let resp = self.client.search_points(req).await.map_err(qe)?;
        Ok(resp
            .result
            .into_iter()
            .map(|p| VecHit {
                score: p.score,
                payload: serde_json::to_value(&p.payload).unwrap_or(Json::Null),
            })
            .collect())
    }
}
