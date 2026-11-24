//! The seam the framework reads an organization's own records through.
//!
//! Records an organization owns directly (rather than through one of its
//! teams) live in the application's tables, which the framework cannot query:
//! Rust's orphan rules keep a framework table and an application table out of
//! one Diesel query. Granting a team access to such a record still needs two
//! answers only the application has, whether a record id is this
//! organization's and what the record is called, so the application describes
//! each kind of record it shares with one [`ResourceKind`] and hands it to
//! [`Options::resource_kind`](crate::tenancy::Options::resource_kind).
//!
//! Resolving access needs none of this: an application loads the record it is
//! about to act on and passes its organization and id to
//! [`resolve_resource_access`](crate::tenancy::resolve_resource_access).

use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use diesel::QueryResult;
use diesel_async::AsyncPgConnection;
use uuid::Uuid;

use crate::http::FieldOption;
use crate::roles::{Action, Permission, RoleSet};

/// What a [`ResourceKind`] call returns.
pub type ResourceKindFuture<'a, Output> =
    Pin<Box<dyn Future<Output = QueryResult<Output>> + Send + 'a>>;

/// One kind of record an organization owns directly, described for sharing.
///
/// An implementation reads the application's own table and nothing else. Both
/// methods answer only the records `organization_id` owns, and leave out an id
/// that is another organization's exactly as they leave out one that never
/// existed, which is what makes a grant naming another tenant's record answer
/// `404` like a missing one.
///
/// Boxed rather than an `async fn`, because a trait with one is not
/// dyn-compatible and the registry holds every kind behind `dyn`.
///
/// ```ignore
/// #[derive(Debug)]
/// struct Machines;
///
/// impl ResourceKind for Machines {
///     fn name(&self) -> &str {
///         "Machine"
///     }
///
///     fn find<'a>(
///         &'a self,
///         connection: &'a mut AsyncPgConnection,
///         organization_id: Uuid,
///         resource_ids: &'a [Uuid],
///     ) -> ResourceKindFuture<'a, Vec<FieldOption>> {
///         Box::pin(async move {
///             let rows: Vec<(Uuid, String)> = machines::table
///                 .filter(machines::organization_id.eq(organization_id))
///                 .filter(machines::id.eq_any(resource_ids))
///                 .select((machines::id, machines::name))
///                 .for_key_share()
///                 .load(connection)
///                 .await?;
///             Ok(rows
///                 .into_iter()
///                 .map(|(value, label)| FieldOption { value, label })
///                 .collect())
///         })
///     }
///
///     fn options<'a>(...) -> ResourceKindFuture<'a, Vec<FieldOption>> { ... }
/// }
/// ```
pub trait ResourceKind: fmt::Debug + Send + Sync + 'static {
    /// The model these records are, spelled exactly as `roles.yml` names it.
    ///
    /// It is the subject of every permission on them, so a member's
    /// `Machine.update` is what lets them change a shared machine.
    fn name(&self) -> &str;

    /// The records among `resource_ids` the organization owns, with their names.
    ///
    /// Ids the organization does not own are left out, and the order of the
    /// answer does not matter. The query must lock what it answers
    /// `FOR KEY SHARE`: a grant is written in the transaction that asked, and
    /// the lock is what makes a record deleted at the same instant either
    /// vanish before the grant is written (refused as missing) or wait until
    /// the grant committed, when the delete's own
    /// [`forget_resource`](crate::tenancy::forget_resource) removes it.
    ///
    /// # Errors
    /// The query's own error.
    fn find<'a>(
        &'a self,
        connection: &'a mut AsyncPgConnection,
        organization_id: Uuid,
        resource_ids: &'a [Uuid],
    ) -> ResourceKindFuture<'a, Vec<FieldOption>>;

    /// The organization's records of this kind a picker offers, at most `limit`.
    ///
    /// `search`, when present, narrows them to the ones a person is typing
    /// the name of; ordered by name either way.
    ///
    /// # Errors
    /// The query's own error.
    fn options<'a>(
        &'a self,
        connection: &'a mut AsyncPgConnection,
        organization_id: Uuid,
        search: Option<&'a str>,
        limit: i64,
    ) -> ResourceKindFuture<'a, Vec<FieldOption>>;
}

/// The kinds of record an application shares, by name.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResourceKinds {
    kinds: BTreeMap<String, Arc<dyn ResourceKind>>,
}

impl ResourceKinds {
    /// Adds a kind.
    ///
    /// # Panics
    /// Panics when another kind already took the name, which is a mistake in
    /// how the application composed its router.
    pub(crate) fn register(&mut self, kind: Arc<dyn ResourceKind>) {
        let name = kind.name().to_owned();
        assert!(
            !self.kinds.contains_key(&name),
            "the resource kind {name:?} is registered twice",
        );
        self.kinds.insert(name, kind);
    }

    /// Checks every kind against the compiled `roles.yml`.
    ///
    /// A kind is a model, so its permissions must be in the vocabulary: a kind
    /// `roles.yml` never names could be granted to a team and then do nothing,
    /// because no role could ever carry an action on it.
    ///
    /// # Panics
    /// Panics naming the first kind `roles.yml` does not name as a model, or
    /// one spelled like a framework subject.
    pub(crate) fn validate(&self, roles: &RoleSet) {
        for name in self.kinds.keys() {
            assert!(
                !name.contains("::"),
                "the resource kind {name:?} is spelled like a framework subject; \
                 a kind is one of the application's own models",
            );
            assert!(
                roles.is_permission(&Permission::new(name.as_str(), Action::Read)),
                "the resource kind {name:?} is not a model config/roles.yml names; \
                 add it to a role there so its records can be acted on",
            );
        }
    }

    /// The kind called `name`, if the application shares one.
    pub(crate) fn get(&self, name: &str) -> Option<&dyn ResourceKind> {
        self.kinds.get(name).map(AsRef::as_ref)
    }

    /// Every kind, ordered by name.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &dyn ResourceKind> {
        self.kinds.values().map(AsRef::as_ref)
    }
}
