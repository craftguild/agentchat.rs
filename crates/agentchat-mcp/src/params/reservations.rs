//! Cooperative leases require exact generations, with policy in the core.

use super::*;
input!(Reserve,Option<Uuid>,{
    /// Complete atomic group of supported logical relative-path patterns.
    #[schemars(length(min=1,max=32))] pub patterns:Vec<String>,
    /// TTL in seconds, default 600; 30 through 3,600.
    #[schemars(range(min=30,max=3600))] pub ttl:Option<u32>,
    /// Optional explanation, maximum 1,024 UTF-8 bytes.
    #[schemars(length(max=1024))] pub reason:Option<String>,
});
input!(Reservations,Option<Uuid>,{
    /// Optional retained owner handle.
    pub owner:Option<String>,
    /// Include retained released/expired groups.
    #[serde(default)] pub all:bool,
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))] pub limit:Option<usize>,
    /// Decimal-string acquisition-order continuation cursor.
    #[serde(default)] #[schemars(with="String")] pub after:Cursor,
});
input!(Lease,Option<Uuid>,{
    /// Exact retained reservation UUID.
    pub reservation_id:Uuid,
    /// Positive acquisition generation; never revives a terminal group.
    #[schemars(range(min=1))] pub generation:i64,
});
