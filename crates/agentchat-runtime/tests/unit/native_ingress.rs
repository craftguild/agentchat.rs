//! Actual ingress collector preserves control and query fences.
use super::*;
fn mutation() -> Command {
    let (reply, _) = oneshot::channel();
    Command::Call {
        request_id: None,
        queued_at: Instant::now(),
        connection: Uuid::new_v4(),
        call: Box::new(Call::Mutate {
            request_id: Uuid::new_v4(),
            actor: agentchat_protocol::rpc::ActorReference::Native {
                context_id: Uuid::new_v4(),
            },
            command: agentchat_core::application::Mutation::Read {
                message_ids: vec![Uuid::new_v4()],
            },
        }),
        bound: None,
        reply,
    }
}
#[tokio::test]
async fn query_gone_and_stop_stay_fifo_fences_and_do_not_consume_their_successor() {
    for kind in 0..3 {
        let (send, mut receive) = mpsc::channel(64);
        send.send(mutation()).await.ok().unwrap();
        let (reply, _) = oneshot::channel();
        let fence = match kind {
            0 => Command::Call {
                request_id: None,
                queued_at: Instant::now(),
                connection: Uuid::new_v4(),
                call: Box::new(Call::InternalHealth),
                bound: None,
                reply,
            },
            1 => Command::Gone(Uuid::new_v4()),
            _ => {
                let (reply, _) = oneshot::channel();
                Command::Stop(reply)
            }
        };
        send.send(fence).await.ok().unwrap();
        send.send(mutation()).await.ok().unwrap();
        let mut carried = None;
        assert_eq!(collect(None, &mut receive, &mut carried, 8).len(), 1);
        assert!(matches!(
            (&carried, kind),
            (Some(Command::Call { .. }), 0)
                | (Some(Command::Gone(_)), 1)
                | (Some(Command::Stop(_)), 2)
        ));
        assert!(collect(None, &mut receive, &mut carried, 8).is_empty());
        carried.take();
        assert_eq!(collect(None, &mut receive, &mut carried, 8).len(), 1);
        assert!(carried.is_none());
    }
}
