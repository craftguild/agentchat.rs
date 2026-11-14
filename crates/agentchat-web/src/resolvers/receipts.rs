//! Canonical paged receipt DTOs remain typed through GraphQL presentation.

use super::{Fields, Types};
use agentchat_core::{
    Result,
    model::{DeliveryState, ReadKind},
    read::{
        IdentityLabel, MessageDetail, MessageView, Page, ReceiptDelivery, ReceiptRead,
        RecipientReceipt,
    },
};
use agentchat_viewer_contract as contract;
use chigiri::{FieldContext, Id};

impl contract::MessageDetailResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn message<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageDetail,
    ) -> Result<MessageView> {
        Ok(p.message.clone())
    }
    async fn receipts<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a MessageDetail,
    ) -> Result<Page<RecipientReceipt>> {
        Ok(p.receipts.clone())
    }
}

impl contract::ReceiptPageResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn items<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<RecipientReceipt>,
    ) -> Result<Vec<RecipientReceipt>> {
        Ok(p.items.clone())
    }
    async fn next_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<RecipientReceipt>,
    ) -> Result<Option<String>> {
        Ok(p.next_cursor.map(|c| c.get().to_string()))
    }
    async fn scan_cursor<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<RecipientReceipt>,
    ) -> Result<String> {
        Ok(p.scan_cursor.get().to_string())
    }
    async fn head<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a Page<RecipientReceipt>,
    ) -> Result<String> {
        Ok(p.head.get().to_string())
    }
}

impl contract::RecipientReceiptResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn recipient<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a RecipientReceipt,
    ) -> Result<IdentityLabel> {
        Ok(p.recipient.clone())
    }
    async fn delivery<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a RecipientReceipt,
    ) -> Result<Option<ReceiptDelivery>> {
        Ok(p.delivery.clone())
    }
    async fn reads<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a RecipientReceipt,
    ) -> Result<Vec<ReceiptRead>> {
        Ok(p.reads.clone())
    }
}

impl contract::DeliveryReceiptResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn delivery_id<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<Id> {
        Ok(Id::new(p.delivery_id.to_string()))
    }
    async fn state<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<contract::DeliveryState> {
        Ok(match p.state {
            DeliveryState::Pending => contract::DeliveryState::Pending,
            DeliveryState::Processing => contract::DeliveryState::Processing,
            DeliveryState::Delivered => contract::DeliveryState::Delivered,
            DeliveryState::FailedTimeout => contract::DeliveryState::FailedTimeout,
            DeliveryState::FailedLeft => contract::DeliveryState::FailedLeft,
            DeliveryState::DeliveredLate => contract::DeliveryState::DeliveredLate,
        })
    }
    async fn queued_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<String> {
        Ok(p.history.queued_at.to_string())
    }
    async fn processing_started_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<Option<String>> {
        Ok(p.history.processing_started_at.map(|at| at.to_string()))
    }
    async fn deadline_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<Option<String>> {
        Ok(p.history.deadline_at.map(|at| at.to_string()))
    }
    async fn timed_out_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<Option<String>> {
        Ok(p.history.timed_out_at.map(|at| at.to_string()))
    }
    async fn delivered_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<Option<String>> {
        Ok(p.history.delivered_at.map(|at| at.to_string()))
    }
    async fn left_at<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptDelivery,
    ) -> Result<Option<String>> {
        Ok(p.history.left_at.map(|at| at.to_string()))
    }
}

impl contract::ReceiptReadResolver<bool, Types, agentchat_core::Error> for Fields {
    async fn kind<'a>(
        &'a self,
        _: FieldContext<'a, bool>,
        p: &'a ReceiptRead,
    ) -> Result<contract::ReadKind> {
        Ok(match p.kind {
            ReadKind::Automatic => contract::ReadKind::Automatic,
            ReadKind::Explicit => contract::ReadKind::Explicit,
        })
    }
    async fn at<'a>(&'a self, _: FieldContext<'a, bool>, p: &'a ReceiptRead) -> Result<String> {
        Ok(p.at.to_string())
    }
}
