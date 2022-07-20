// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Model types for IP Pools and the CIDR blocks therein.

use crate::db::collection_insert::DatastoreCollection;
use crate::db::model::Name;
use crate::db::schema::ip_pool;
use crate::db::schema::ip_pool_range;
use crate::external_api::params;
use crate::external_api::shared::IpRange;
use chrono::DateTime;
use chrono::Utc;
use db_macros::Resource;
use diesel::Selectable;
use ipnetwork::IpNetwork;
use omicron_common::api::external;
use std::net::IpAddr;
use uuid::Uuid;

/// An IP Pool is a collection of IP addresses external to the rack.
#[derive(Queryable, Insertable, Selectable, Clone, Debug, Resource)]
#[diesel(table_name = ip_pool)]
pub struct IpPool {
    #[diesel(embed)]
    pub identity: IpPoolIdentity,

    /// An optional ID of the project for which this pool is reserved.
    pub project_id: Option<Uuid>,

    /// Child resource generation number, for optimistic concurrency control of
    /// the contained ranges.
    pub rcgen: i64,
}

impl IpPool {
    pub fn new(
        pool_identity: &external::IdentityMetadataCreateParams,
        project_id: Option<Uuid>,
    ) -> Self {
        Self {
            identity: IpPoolIdentity::new(
                Uuid::new_v4(),
                pool_identity.clone(),
            ),
            project_id,
            rcgen: 0,
        }
    }
}

/// A set of updates to an IP Pool
#[derive(AsChangeset)]
#[diesel(table_name = ip_pool)]
pub struct IpPoolUpdate {
    pub name: Option<Name>,
    pub description: Option<String>,
    pub time_modified: DateTime<Utc>,
}

impl From<params::IpPoolUpdate> for IpPoolUpdate {
    fn from(params: params::IpPoolUpdate) -> Self {
        Self {
            name: params.identity.name.map(|n| n.into()),
            description: params.identity.description,
            time_modified: Utc::now(),
        }
    }
}

/// A range of IP addresses for an IP Pool.
#[derive(Queryable, Insertable, Selectable, Clone, Debug)]
#[diesel(table_name = ip_pool_range)]
pub struct IpPoolRange {
    pub id: Uuid,
    pub time_created: DateTime<Utc>,
    pub time_modified: DateTime<Utc>,
    pub time_deleted: Option<DateTime<Utc>>,
    /// First (lowest) address in the range, inclusive.
    pub first_address: IpNetwork,
    /// Last (highest) address in the range, inclusive.
    pub last_address: IpNetwork,
    /// Foreign-key to the `ip_pool` table with the parent pool for this range
    pub ip_pool_id: Uuid,
    /// Foreign-key to the `project` table, with the Project to which this range
    /// is restricted, if any (derived from the `ip_pool` table).
    pub project_id: Option<Uuid>,
    /// The child resource generation number, tracking IP addresses allocated or
    /// used from this range.
    pub rcgen: i64,
}

impl IpPoolRange {
    pub fn new(
        range: &IpRange,
        ip_pool_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Self {
        let now = Utc::now();
        let first_address = range.first_address();
        let last_address = range.last_address();
        // `range` has already been validated to have first address no greater
        // than last address.
        assert!(
            last_address >= first_address,
            "Address ranges must be non-decreasing"
        );
        Self {
            id: Uuid::new_v4(),
            time_created: now,
            time_modified: now,
            time_deleted: None,
            first_address: IpNetwork::from(range.first_address()),
            last_address: IpNetwork::from(range.last_address()),
            ip_pool_id,
            project_id,
            rcgen: 0,
        }
    }
}

impl From<&IpPoolRange> for IpRange {
    fn from(range: &IpPoolRange) -> Self {
        let maybe_range =
            match (range.first_address.ip(), range.last_address.ip()) {
                (IpAddr::V4(first), IpAddr::V4(last)) => {
                    IpRange::try_from((first, last))
                }
                (IpAddr::V6(first), IpAddr::V6(last)) => {
                    IpRange::try_from((first, last))
                }
                (first, last) => {
                    unreachable!(
                        "Expected first/last address of an IP range to \
                    both be of the same protocol version, but first = {:?} \
                    and last = {:?}",
                        first, last,
                    );
                }
            };
        maybe_range
            .expect("Retrieved an out-of-order IP range pair from the database")
    }
}

impl DatastoreCollection<IpPoolRange> for IpPool {
    type CollectionId = uuid::Uuid;
    type GenerationNumberColumn = ip_pool::dsl::rcgen;
    type CollectionTimeDeletedColumn = ip_pool::dsl::time_deleted;
    type CollectionIdColumn = ip_pool_range::dsl::ip_pool_id;
}
