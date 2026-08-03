// ====== 统一错误类型 ======

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// 应用层错误枚举，映射到对应的 HTTP 状态码
#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Forbidden(String),
    NotFound(String),
    Internal(String),
    PayloadTooLarge(String),
    ServiceUnavailable(String),
    Unauthorized(String),
    Conflict(String),
    Gone(String),
    TooManyRequests(String),
    BadGateway(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::BadRequest(msg) => write!(f, "Bad Request: {}", msg),
            AppError::Unauthorized(msg) => write!(f, "Unauthorized: {}", msg),
            AppError::Forbidden(msg) => write!(f, "Forbidden: {}", msg),
            AppError::NotFound(msg) => write!(f, "Not Found: {}", msg),
            AppError::Conflict(msg) => write!(f, "Conflict: {}", msg),
            AppError::Gone(msg) => write!(f, "Gone: {}", msg),
            AppError::TooManyRequests(msg) => write!(f, "Too Many Requests: {}", msg),
            AppError::Internal(msg) => write!(f, "Internal Server Error: {}", msg),
            AppError::PayloadTooLarge(msg) => write!(f, "Payload Too Large: {}", msg),
            AppError::BadGateway(msg) => write!(f, "Bad Gateway: {}", msg),
            AppError::ServiceUnavailable(msg) => write!(f, "Service Unavailable: {}", msg),
        }
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Gone(msg) => (StatusCode::GONE, msg.clone()),
            AppError::TooManyRequests(msg) => (StatusCode::TOO_MANY_REQUESTS, msg.clone()),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            AppError::PayloadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg.clone()),
            AppError::BadGateway(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
            AppError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
        };
        (status, body).into_response()
    }
}

// From impls for automatic ? propagation

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("[DB] {}", e);
        AppError::Internal("数据库操作失败，请稍后重试".to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        tracing::error!("[IO] {}", e);
        AppError::Internal("文件系统操作失败".to_string())
    }
}

impl From<Box<dyn std::error::Error>> for AppError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        tracing::error!("[Error] {}", e);
        AppError::Internal(e.to_string())
    }
}

// Convenience constructors

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self { AppError::BadRequest(msg.into()) }
    pub fn forbidden(msg: impl Into<String>) -> Self { AppError::Forbidden(msg.into()) }
    pub fn not_found(msg: impl Into<String>) -> Self { AppError::NotFound(msg.into()) }
    pub fn conflict(msg: impl Into<String>) -> Self { AppError::Conflict(msg.into()) }
    pub fn gone(msg: impl Into<String>) -> Self { AppError::Gone(msg.into()) }
    pub fn internal(msg: impl Into<String>) -> Self { AppError::Internal(msg.into()) }
    pub fn payload_too_large(msg: impl Into<String>) -> Self { AppError::PayloadTooLarge(msg.into()) }
    pub fn service_unavailable(msg: impl Into<String>) -> Self { AppError::ServiceUnavailable(msg.into()) }
}

/// 便捷类型别名
pub type AppResult<T> = Result<T, AppError>;
