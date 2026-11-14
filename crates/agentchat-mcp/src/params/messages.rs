//! Message, delivery and finite event inputs; arrays are exact atomic ID sets.

use super::*;
input!(Post,Option<Uuid>,{
    /// Optional channel slug; omission resolves to the project's primary default.
    pub channel:Option<String>,
    /// Complete literal body, maximum 65,536 UTF-8 bytes; never evaluated.
    #[schemars(length(max=65536))] pub body:String,
});
input!(Reply,Option<Uuid>,{
    /// Exact complete bodies explicitly read, atomically before this reply.
    #[schemars(length(max=200))] pub read_message_ids:Option<Vec<Uuid>>,
    /// Exact root UUID; replies to replies are invalid.
    pub root_message_id:Uuid,
    /// Complete literal body, maximum 65,536 UTF-8 bytes.
    #[schemars(length(max=65536))] pub body:String,
});
input!(Message,Option<Uuid>,{
    /// Exact immutable message UUID.
    pub message_id:Uuid,
    /// Maximum nested receipt items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))] pub limit:Option<usize>,
    /// Receipt continuation cursor, not a read high-water mark.
    #[serde(default)] #[schemars(with="String")] pub after:Cursor,
});
input!(History,Option<Uuid>,{
    /// Optional channel slug, consistent with any thread root.
    pub channel:Option<String>,
    /// Optional exact root message UUID.
    pub thread:Option<Uuid>,
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))] pub limit:Option<usize>,
    /// Decimal-string creation-order cursor.
    #[serde(default)] #[schemars(with="String")] pub after:Cursor,
});
input!(Search,Option<Uuid>,{
    /// Case-sensitive literal substring, maximum 1,024 UTF-8 bytes.
    #[schemars(length(max=1024))] pub query:String,
    /// Optional channel slug.
    pub channel:Option<String>,
    /// Optional exact root UUID.
    pub thread:Option<Uuid>,
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))] pub limit:Option<usize>,
    /// Decimal-string continuation cursor.
    #[serde(default)] #[schemars(with="String")] pub after:Cursor,
});
input!(Read,Option<Uuid>,{
    /// Exact message UUIDs actually read; validated as one atomic set.
    #[schemars(length(min=1,max=200))] pub message_ids:Vec<Uuid>,
});
input!(Events,Option<Uuid>,{
    /// Optional channel slug.
    pub channel:Option<String>,
    /// Maximum page items, default 50; 1 through 200.
    #[schemars(range(min=1,max=200))] pub limit:Option<usize>,
    /// Decimal-string event scan cursor; omission starts at zero.
    #[serde(default)] #[schemars(with="String")] pub after:Cursor,
});
