//! Request-associated decoding and poisoning on malformed concrete read DTOs.
use agentchat_core::{
    ErrorCode,
    read::{ChannelListCondition, ChannelPage, PageRequest, requests},
};
use agentchat_protocol::{
    framing,
    rpc::{Client, Id, Reply, Request, success},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
use tokio::{io::BufReader, net::UnixListener};
use uuid::Uuid;

fn key(request: &Request) -> Uuid {
    let Some(Id::Text(id)) = &request.id else {
        panic!("UUID correlation required")
    };
    Uuid::parse_str(id).unwrap()
}

async fn fixture(payload: Value) -> (tempfile::TempDir, Client, tokio::task::JoinHandle<()>) {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("typed.sock");
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let project = Uuid::new_v4();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello: Request = framing::read(&mut read).await.unwrap().unwrap();
        framing::write(
            &mut write,
            &Reply::success(
                hello.id.clone(),
                key(&hello),
                project,
                json!({"version":1,"project_id":project,"instance_id":Uuid::new_v4()}),
            ),
        )
        .await
        .unwrap();
        let request: Request = framing::read(&mut read).await.unwrap().unwrap();
        assert_eq!(request.method, "application.query");
        assert_eq!(request.params["query"]["operation"], "channels");
        framing::write(
            &mut write,
            &Reply::success(request.id.clone(), key(&request), project, payload),
        )
        .await
        .unwrap();
        assert!(
            framing::read::<_, Request>(&mut read)
                .await
                .unwrap()
                .is_none(),
            "a poisoned client cannot send another request"
        );
    });
    let client = Client::connect(&path, project).await.unwrap();
    (directory, client, task)
}

#[tokio::test]
async fn an_empty_page_retains_the_requested_channel_type_without_shape_guessing() {
    let (_directory, mut client, task) = fixture(json!({
        "items":[],"next_cursor":null,"scan_cursor":"9007199254740993","head":"9007199254740993",
        "as_of":"2026-10-26T00:00:00.000Z"
    }))
    .await;
    let page: ChannelPage = success(
        client
            .read(
                Uuid::new_v4(),
                None,
                requests::Channels {
                    condition: ChannelListCondition::Active,
                    page: PageRequest::default(),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page.page.head.get(), 9_007_199_254_740_993);
    assert!(page.page.items.is_empty());
    drop(client);
    task.await.unwrap();
}

#[tokio::test]
async fn a_different_page_item_shape_is_rejected_and_cannot_desynchronize_the_client() {
    let (_directory, mut client, task) = fixture(json!({
        "items":[{"id":Uuid::new_v4(),"body":"this is not a channel"}],
        "next_cursor":null,"scan_cursor":"1","head":"1",
        "as_of":"2026-10-26T00:00:00.000Z"
    }))
    .await;
    let error = client
        .read(
            Uuid::new_v4(),
            None,
            requests::Channels {
                condition: ChannelListCondition::Active,
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        client.health().await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    drop(client);
    task.await.unwrap();
}
