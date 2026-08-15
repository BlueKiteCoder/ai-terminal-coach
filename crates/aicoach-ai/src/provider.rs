use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    AiError, AiResult, AnalysisInput, AnalysisResult, ChatRequest, ChatResponse, ChatStream,
    CommandCompletionRequest, CompletionResult,
};

#[async_trait]
pub trait AiProvider: Send + Sync {
    async fn chat(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> AiResult<ChatResponse>;

    async fn stream_chat(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> AiResult<ChatStream>;

    async fn analyze_command(
        &self,
        request: AnalysisInput,
        cancellation: CancellationToken,
    ) -> AiResult<AnalysisResult>;

    async fn complete_command(
        &self,
        request: CommandCompletionRequest,
        cancellation: CancellationToken,
    ) -> AiResult<CompletionResult>;
}

/// Explicit provider for disabled/offline configurations.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopAiProvider;

#[async_trait]
impl AiProvider for NoopAiProvider {
    async fn chat(
        &self,
        _request: ChatRequest,
        _cancellation: CancellationToken,
    ) -> AiResult<ChatResponse> {
        Err(AiError::Offline)
    }

    async fn stream_chat(
        &self,
        _request: ChatRequest,
        _cancellation: CancellationToken,
    ) -> AiResult<ChatStream> {
        Err(AiError::Offline)
    }

    async fn analyze_command(
        &self,
        _request: AnalysisInput,
        _cancellation: CancellationToken,
    ) -> AiResult<AnalysisResult> {
        Err(AiError::Offline)
    }

    async fn complete_command(
        &self,
        _request: CommandCompletionRequest,
        _cancellation: CancellationToken,
    ) -> AiResult<CompletionResult> {
        Err(AiError::Offline)
    }
}
