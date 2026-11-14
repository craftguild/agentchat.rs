//! Display names use path components rather than home or separator conventions.
use agentchat_web::ViewerContext;
use std::path::Path;

#[test]
fn launch_directory_name_is_only_the_last_component() {
    for (path, expected) in [
        ("/Users/alice/projects/chat", "chat"),
        ("/Volumes/data/lab05", "lab05"),
        ("/tmp/<chat> & 日本語", "<chat> & 日本語"),
        ("/", "Root"),
    ] {
        let context = ViewerContext::from_directory(Path::new(path));
        assert_eq!(context.directory_name.as_deref(), Some(expected));
        let json = serde_json::to_string(&context).unwrap();
        assert!(!json.contains("/Users/alice"));
    }
}
