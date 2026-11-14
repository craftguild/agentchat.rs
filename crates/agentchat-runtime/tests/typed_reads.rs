//! Concrete request DTOs survive real SeaORM snapshots and local RPC decoding.
mod support;

use agentchat_core::{
    ErrorCode,
    application::{Application, Mutation},
    model::Context,
    read::{
        ChannelListCondition, ChannelPage, MessageDetail, MessageView, Page, PageRequest,
        ReadRequest, WhoamiView, requests,
    },
};
use agentchat_protocol::{
    health::RuntimeWhoami,
    rpc::{Client, success},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt::Debug, sync::Arc};
use support::runtime::{Running, join, mutate, options};
use uuid::Uuid;

async fn parity<R>(app: &Application, client: &mut Client, context: &Context, request: R)
where
    R: ReadRequest + Clone,
    R::Response<WhoamiView>: Serialize + Debug,
    R::Response<RuntimeWhoami>: DeserializeOwned + Serialize,
{
    let direct = app.read(Some(context), request.clone()).await.unwrap();
    let id = Uuid::new_v4();
    let envelope = client
        .read(id, Some(context.context_id), request)
        .await
        .unwrap();
    assert_eq!(envelope.request_id, id);
    assert_eq!(envelope.project_id, Some(context.project_id));
    assert_eq!(envelope.version, 1);
    // This is the assertion/presentation boundary, not DTO reconstruction.
    let mut wire = serde_json::to_value(success(envelope).unwrap()).unwrap();
    if let Some(object) = wire.as_object_mut() {
        object.remove("runtime");
    }
    assert_eq!(serde_json::to_value(direct).unwrap(), wire);
}

async fn viewer_parity<R>(
    app: &Application,
    observer: &mut Client,
    bound: &mut Client,
    context: &Context,
    request: R,
) where
    R: ReadRequest + Clone,
    R::Response<WhoamiView>: Serialize + Debug,
    R::Response<RuntimeWhoami>: DeserializeOwned + Serialize + Debug,
{
    let direct_error = app.read(Some(context), request.clone()).await.unwrap_err();
    assert_eq!(direct_error.code, ErrorCode::InvalidArgument);
    let bound_error = bound
        .read(Uuid::new_v4(), Some(context.context_id), request.clone())
        .await
        .unwrap_err();
    assert_eq!(bound_error.code, ErrorCode::InvalidArgument);
    let direct = app.read(None, request.clone()).await.unwrap();
    let envelope = observer.read(Uuid::new_v4(), None, request).await.unwrap();
    assert_eq!(
        serde_json::to_value(direct).unwrap(),
        serde_json::to_value(success(envelope).unwrap()).unwrap()
    );
}

#[tokio::test]
async fn every_finite_read_keeps_its_concrete_type_and_existing_wire_projection() {
    let r = Running::new(options()).await;
    let context = r.context("typed-reads").await;
    let mut client = r.client().await;
    join(&mut client, &context, "typed-reader").await;
    let mut observer = r.client().await;
    let posted = mutate(
        &mut client,
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "full 日本語\0\nbody".into(),
        },
    )
    .await;
    let id = Uuid::parse_str(posted["message_id"].as_str().unwrap()).unwrap();
    let store = r.f.manager.open(&r.project).await.unwrap();
    let app = Application::new(store, r.clock.clone(), Arc::new(support::SyntheticHost));
    let before = app.health().await.unwrap();
    let page = PageRequest::default();
    viewer_parity(
        &app,
        &mut observer,
        &mut client,
        &context,
        requests::ViewerHead,
    )
    .await;
    viewer_parity(
        &app,
        &mut observer,
        &mut client,
        &context,
        requests::RootWindow {
            channel: "dev".into(),
            window: Default::default(),
        },
    )
    .await;
    viewer_parity(
        &app,
        &mut observer,
        &mut client,
        &context,
        requests::PinnedRoot {
            channel: "dev".into(),
            root_message_id: id,
            snapshot: None,
        },
    )
    .await;
    viewer_parity(
        &app,
        &mut observer,
        &mut client,
        &context,
        requests::ReplyWindow {
            channel: "dev".into(),
            root_message_id: id,
            window: Default::default(),
        },
    )
    .await;
    viewer_parity(
        &app,
        &mut observer,
        &mut client,
        &context,
        requests::ViewerChanges {
            changes: agentchat_core::read::ChangeRequest {
                after: agentchat_core::read::Checkpoint::start(app.project_id()),
                through: None,
                limit: None,
            },
        },
    )
    .await;

    parity(&app, &mut client, &context, requests::Whoami).await;
    parity(&app, &mut client, &context, requests::Agents { page }).await;
    parity(
        &app,
        &mut client,
        &context,
        requests::Channels {
            condition: ChannelListCondition::Active,
            page,
        },
    )
    .await;
    parity(
        &app,
        &mut client,
        &context,
        requests::ChannelMembers {
            channel: "dev".into(),
            page,
        },
    )
    .await;
    parity(
        &app,
        &mut client,
        &context,
        requests::Message {
            message_id: id,
            receipts: page,
        },
    )
    .await;
    parity(
        &app,
        &mut client,
        &context,
        requests::History {
            channel: None,
            thread: None,
            page,
        },
    )
    .await;
    parity(
        &app,
        &mut client,
        &context,
        requests::Search {
            query: "日本語".into(),
            channel: None,
            thread: None,
            page,
        },
    )
    .await;
    parity(&app, &mut client, &context, requests::Inbox { page }).await;
    parity(
        &app,
        &mut client,
        &context,
        requests::Events {
            channel: None,
            page,
        },
    )
    .await;
    parity(
        &app,
        &mut client,
        &context,
        requests::Reservations {
            owner: None,
            all: true,
            page,
        },
    )
    .await;

    // These annotations also exercise the specific public return types. No
    // enum match or JSON deserialization is available/needed at the consumer.
    let whoami: RuntimeWhoami = success(
        client
            .read(Uuid::new_v4(), Some(context.context_id), requests::Whoami)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(whoami.application.actor.unwrap().actor.joined);
    let channels: ChannelPage = success(
        client
            .read(
                Uuid::new_v4(),
                None,
                requests::Channels {
                    condition: ChannelListCondition::Active,
                    page,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(channels.page.items[0].channel.name, "dev");
    let history: Page<MessageView> = success(
        client
            .read(
                Uuid::new_v4(),
                None,
                requests::History {
                    channel: None,
                    thread: None,
                    page,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(history.items[0].message.body, "full 日本語\0\nbody");
    let detail: MessageDetail = success(
        client
            .read(
                Uuid::new_v4(),
                None,
                requests::Message {
                    message_id: id,
                    receipts: page,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(detail.receipts.items.is_empty());
    let binding = client.native_binding(context.context_id).await.unwrap();
    assert!(binding.agent.unwrap().agent.joined);
    let mut unbound = r.client().await;
    let health = unbound.health().await.unwrap();
    assert_eq!(health.application, before);
    assert_eq!(health.runtime.instance_id, client.instance_id());
    assert_eq!(health.runtime.native_delivery.jobs, 0);
    assert_eq!(
        app.health().await.unwrap(),
        before,
        "reads cannot change head, participants, ACKs or explicit receipts"
    );
    r.stop().await;
}
