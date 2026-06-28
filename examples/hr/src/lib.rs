//! A standalone HR example built **directly** on the `es-capture` spine.
//!
//! It models two aggregates — [`Person`] and [`Employment`] — as
//! specialisations of the generic `CaptureAggregate`, with **no aggregate code
//! of their own**. Each differs only in its attribute schema (which paths are
//! plaintext vs. encrypted-per-subject).
//!
//! The headline behaviour is **cross-aggregate crypto-shredding**: the same
//! human is the same data subject in both aggregates (bound under `/self` in
//! `Person` and `/employee` in `Employment`), so a single right-to-erasure
//! (`KeyStore::delete_key`) makes their PII permanently unreadable in *both*.
//!
//! The whole stack runs in memory (`InMemoryEventRepository` +
//! `InMemoryKeyStore`); there is no Postgres and no HTTP server.

use std::collections::BTreeMap;
use std::sync::Arc;

use cqrs_es::persist::{
    PersistedEventRepository, PersistedEventStore, PersistenceError, ReplayStream, SerializedEvent,
    SerializedSnapshot,
};
use cqrs_es::{Aggregate, CqrsFramework, Query};
use cqrs_es_crypto::{
    CryptoShreddingEventRepository, FieldCipher, InMemoryEventRepository, InMemoryKeyStore,
    KeyStore, PiiEventCodec,
};
use serde_json::Value;
use uuid::Uuid;

use es_capture::aggregate::{CaptureAggregate, CaptureConfig, CaptureEvent, CaptureServices};
use es_capture::attribute_schema::{AttributeEntry, AttributeSchema, PiiClass};
use es_capture::attributes_set_codec::AttributesSetCodec;
use es_capture::schema_validator::NoOpValidator;

use jsonptr::PointerBuf;

// ── Aggregates ────────────────────────────────────────────────────────────────
//
// Both aggregates are just the generic capture spine with a different `TYPE`.

/// Selects the `"Person"` aggregate type.
pub struct PersonConfig;
impl CaptureConfig for PersonConfig {
    const TYPE: &'static str = "Person";
}
/// The person aggregate: identity + sensitive personal data.
pub type Person = CaptureAggregate<PersonConfig>;

/// Selects the `"Employment"` aggregate type.
pub struct EmploymentConfig;
impl CaptureConfig for EmploymentConfig {
    const TYPE: &'static str = "Employment";
}
/// The employment aggregate: the relationship between a person and the org.
pub type Employment = CaptureAggregate<EmploymentConfig>;

// ── Role paths ──────────────────────────────────────────────────────────────
//
// The role path is the slot a subject is bound to and doubles as the crypto
// label. A `Person` instance is about a single human, so its role path is
// `/self`; in `Employment` the same human plays the `/employee` role.

/// Role path under which the data subject is bound in a [`Person`] aggregate.
pub const PERSON_ROLE: &str = "/self";
/// Role path under which the data subject is bound in an [`Employment`] aggregate.
pub const EMPLOYMENT_ROLE: &str = "/employee";

/// Parse a `&str` into a [`PointerBuf`], panicking on an invalid literal.
///
/// # Panics
/// Panics if `s` is not a valid RFC6901 JSON Pointer.
#[must_use]
pub fn ptr(s: &str) -> PointerBuf {
    s.parse().expect("valid JSON pointer literal")
}

/// Build a flat `SetAttributes` change map from `(path, value)` pairs.
#[must_use]
pub fn attrs(pairs: Vec<(&str, Value)>) -> BTreeMap<PointerBuf, Value> {
    pairs.into_iter().map(|(p, v)| (ptr(p), v)).collect()
}

// ── Attribute schemas ─────────────────────────────────────────────────────────

/// Person schema in **explicit** mode.
///
/// Every PII field is a `Secret` bound to the `/self` subject; `/self/country`
/// is the one plaintext field. Any path not listed is rejected.
#[must_use]
pub fn person_attribute_schema() -> AttributeSchema {
    let subject = ptr(PERSON_ROLE);
    let secret = |p: &str| {
        (
            ptr(p),
            AttributeEntry::new(PiiClass::Secret {
                subject: subject.clone(),
            }),
        )
    };
    let paths = BTreeMap::from([
        secret("/self/firstName"),
        secret("/self/lastName"),
        secret("/self/dateOfBirth"),
        secret("/self/nationalInsuranceNumber"),
        (
            ptr("/self/country"),
            AttributeEntry::new(PiiClass::Plaintext),
        ),
    ]);
    AttributeSchema::new(paths, None)
}

/// Employment schema mixing explicit secrets with a plaintext prefix.
///
/// The `/employee/*` financial fields are `Secret` bound to the `/employee`
/// subject; the entire `/employment` subtree (the person reference and
/// non-sensitive role data) is plaintext via a prefix rule.
#[must_use]
pub fn employment_attribute_schema() -> AttributeSchema {
    let subject = ptr(EMPLOYMENT_ROLE);
    let secret = |p: &str| {
        (
            ptr(p),
            AttributeEntry::new(PiiClass::Secret {
                subject: subject.clone(),
            }),
        )
    };
    let paths = BTreeMap::from([
        secret("/employee/salary"),
        secret("/employee/bankAccountNumber"),
        secret("/employee/bankSortCode"),
    ]);
    AttributeSchema::new(paths, None).with_plaintext_prefixes(vec![ptr("/employment")])
}

// ── Services ──────────────────────────────────────────────────────────────────
//
// No decision engine in this example: capture runs and emits `AttributesSet`,
// but no `WorkflowEvaluated`.

/// Capture services for the [`Person`] aggregate.
#[must_use]
pub fn person_services() -> CaptureServices {
    CaptureServices::without_decision_engine(
        Arc::new(NoOpValidator),
        Arc::new(person_attribute_schema()),
    )
}

/// Capture services for the [`Employment`] aggregate.
#[must_use]
pub fn employment_services() -> CaptureServices {
    CaptureServices::without_decision_engine(
        Arc::new(NoOpValidator),
        Arc::new(employment_attribute_schema()),
    )
}

// ── SharedRepo ────────────────────────────────────────────────────────────────

/// A cheaply-clonable handle to a single in-memory event log.
///
/// `InMemoryEventRepository` owns a `Mutex<Vec<_>>` and is neither `Clone` nor
/// shareable, but [`CqrsFramework`] takes ownership of the store. Wrapping it in
/// an `Arc` lets the same event log back the `Person` framework, the
/// `Employment` framework, and a separate read-only reader.
#[derive(Clone)]
pub struct SharedRepo(pub Arc<InMemoryEventRepository>);

impl SharedRepo {
    /// Create an empty shared event log.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(InMemoryEventRepository::default()))
    }
}

impl Default for SharedRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl PersistedEventRepository for SharedRepo {
    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        self.0.get_events::<A>(aggregate_id).await
    }

    async fn get_last_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
        last_sequence: usize,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        self.0
            .get_last_events::<A>(aggregate_id, last_sequence)
            .await
    }

    async fn get_snapshot<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Option<SerializedSnapshot>, PersistenceError> {
        self.0.get_snapshot::<A>(aggregate_id).await
    }

    async fn persist<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        snapshot_update: Option<(String, Value, usize)>,
    ) -> Result<(), PersistenceError> {
        self.0.persist::<A>(events, snapshot_update).await
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        self.0.stream_events::<A>(aggregate_id).await
    }

    async fn stream_all_events<A: Aggregate>(&self) -> Result<ReplayStream, PersistenceError> {
        self.0.stream_all_events::<A>().await
    }
}

// ── Wiring ────────────────────────────────────────────────────────────────────

/// The crypto-shredding repository used throughout the example.
pub type HrRepo = CryptoShreddingEventRepository<SharedRepo>;

/// A CQRS framework over a capture aggregate, backed by the in-memory
/// crypto-shredding store.
pub type HrCqrs<C> =
    CqrsFramework<CaptureAggregate<C>, PersistedEventStore<HrRepo, CaptureAggregate<C>>>;

/// The assembled HR system: one shared event log and one shared key store back
/// both aggregate frameworks plus a read-only reader.
pub struct HrApp {
    /// Command side for [`Person`] aggregates.
    pub person: HrCqrs<PersonConfig>,
    /// Command side for [`Employment`] aggregates.
    pub employment: HrCqrs<EmploymentConfig>,
    /// The shared key store. Deleting a subject's DEK here is the cross-aggregate
    /// right-to-erasure.
    pub key_store: Arc<dyn KeyStore>,
    /// A read-only crypto repository sharing the same event log and key store,
    /// used by [`read_state`] to fold an aggregate's current view back out.
    pub reader: HrRepo,
}

impl HrApp {
    /// Wire up the in-memory HR system.
    #[must_use]
    pub fn build() -> Self {
        let shared = SharedRepo::new();
        let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
        let codec: Arc<dyn PiiEventCodec> = Arc::new(AttributesSetCodec);

        let person = framework::<PersonConfig>(&shared, &key_store, &codec, person_services());
        let employment =
            framework::<EmploymentConfig>(&shared, &key_store, &codec, employment_services());

        let reader = CryptoShreddingEventRepository::new(
            shared,
            Arc::clone(&key_store),
            FieldCipher::new(),
            Arc::clone(&codec),
        );

        Self {
            person,
            employment,
            key_store,
            reader,
        }
    }
}

fn framework<C: CaptureConfig>(
    shared: &SharedRepo,
    key_store: &Arc<dyn KeyStore>,
    codec: &Arc<dyn PiiEventCodec>,
    services: CaptureServices,
) -> HrCqrs<C> {
    let repo = CryptoShreddingEventRepository::new(
        shared.clone(),
        Arc::clone(key_store),
        FieldCipher::new(),
        Arc::clone(codec),
    );
    let store = PersistedEventStore::new_event_store(repo);
    let queries: Vec<Box<dyn Query<CaptureAggregate<C>>>> = vec![];
    CqrsFramework::new(store, queries, services)
}

/// Fold an aggregate's persisted events back into its accumulated view.
///
/// Reads through the crypto layer, so secret fields are decrypted when the
/// subject's DEK is present and **absent** (redacted to a `/redacted` marker)
/// once it has been shredded.
///
/// # Panics
/// Panics if the events cannot be read or deserialised.
pub async fn read_state<C: CaptureConfig>(reader: &HrRepo, id: Uuid) -> Value {
    let events = reader
        .get_events::<CaptureAggregate<C>>(&id.to_string())
        .await
        .expect("read events");
    let mut agg = CaptureAggregate::<C>::default();
    for event in events {
        let domain: CaptureEvent = serde_json::from_value(event.payload).expect("decode event");
        agg.apply(domain);
    }
    agg.shared_data().clone()
}

#[cfg(test)]
mod tests;
