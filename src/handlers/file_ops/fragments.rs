// ====== file_ops：HTMX 片段与预览（P1-1 拆分） ======
use askama::Template;
use axum::{extract::Extension, response::IntoResponse};
use sqlx::Row;

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::file_ops::core::{
    FileRowData, create_folder_common, delete_to_trash, logical_path, move_core,
    normalize_display_path, normalize_preview_mime, query_files, rename_core, user_file_path,
};
use crate::handlers::utils::{
    bytes_to_mb_string, log_audit, safe_join_sandbox, update_user_used_mb, user_dir_path,
};
use crate::templates::AppTemplate;

#[derive(Template)]
#[template(path = "components/file_table.html")]
struct FileTableFragment {
    files: Vec<FileRowData>,
    current_path: String,
    /// 全局搜索模式（path 为空 + 有搜索词）：结果跨目录，显示所在路径
    has_global_search: bool,
    /// 当前搜索词（排序表头 hx-vals 需带上，否则点击排序即丢失搜索上下文）
    search: String,
}

async fn fallback_file_list(
    pool: sqlx::SqlitePool,
    username: String,
    path: String,
) -> axum::response::Response {
    let files = query_files(&pool, &username, &path, None, None).await;
    let mut resp = AppTemplate(FileTableFragment {
        files,
        current_path: path,
        has_global_search: false,
        search: String::new(),
    })
    .into_response();
    resp.headers_mut().insert(
        "HX-Trigger",
        axum::http::HeaderValue::from_static("quotaRefresh"),
    );
    resp
}

/// HTMX 错误恢复 + 错误提示：列表照常刷新，同时经 HX-Trigger(JSON) 派发 toastError 事件
/// （base.html 监听并弹错误 Toast）——历史实现错误被静默吞掉，用户看到"操作成功"的假象
async fn fallback_file_list_with_error(
    pool: sqlx::SqlitePool,
    username: String,
    path: String,
    msg: &str,
) -> axum::response::Response {
    let files = query_files(&pool, &username, &path, None, None).await;
    let mut resp = AppTemplate(FileTableFragment {
        files,
        current_path: path,
        has_global_search: false,
        search: String::new(),
    })
    .into_response();
    let trigger = format!(
        "{{\"toastError\": {}}}",
        serde_json::to_string(msg).unwrap_or_else(|_| "\"操作失败\"".to_string())
    );
    resp.headers_mut().insert(
        "HX-Trigger",
        axum::http::HeaderValue::from_str(&trigger)
            .unwrap_or(axum::http::HeaderValue::from_static("quotaRefresh")),
    );
    resp
}

// ====== 1. JSON API: 文件列表 ======

struct BreadcrumbPart {
    name: String,
    path: String,
}

#[derive(Template)]
#[template(path = "components/breadcrumbs.html")]
struct BreadcrumbsFragment {
    parts: Vec<BreadcrumbPart>,
}

#[derive(Template)]
#[template(path = "components/upload_form.html")]
struct UploadFormFragment {
    current_path: String,
}

#[derive(Template)]
#[template(path = "components/new_folder_form.html")]
struct NewFolderFormFragment {}

#[derive(Template)]
#[template(path = "components/quota_bar.html")]
struct QuotaFragment {
    used_mb: i64,
    total_mb: i64,
    percent: u32,
}

#[derive(Template)]
#[template(path = "components/rename_form.html")]
struct RenameFormFragment {
    current_path: String,
    old_name: String,
}

#[derive(Template)]
#[template(path = "components/move_form.html")]
struct MoveFormFragment {
    current_path: String,
    name: String,
    dirs: Vec<String>,
}

#[derive(Template)]
#[template(path = "components/preview.html")]
pub struct PreviewFragment {
    file_name: String,
    file_path: String,
    file_size: String,
    mime_type: String,
    is_image: bool,
    is_video: bool,
    is_audio: bool,
    is_pdf: bool,
    is_text: bool,
    content: String,
    /// 用户(视频续播 localStorage key)
    username: String,
    /// Markdown 渲染模式：原始内容 JSON 编码（< 转义防 </script> 逃逸）
    is_markdown: bool,
    markdown_json: String,
    /// 画廊相邻文件（图片模式翻页；空串 = 无）
    prev_path: String,
    prev_name: String,
    next_path: String,
    next_name: String,
    /// 媒体令牌（短时效 + 目录限定）：<img>/<video> 等无 Cookie 场景的 /api/media/ 访问凭证
    media_token: String,
}

/// 画廊导航项（同目录相邻文件）
struct PreviewNav {
    path: String,
    name: String,
}

// ====== HTMX Fragment Handlers ======

/// GET /drive/list
#[tracing::instrument(skip_all)]
pub async fn drive_list_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let path = normalize_display_path(&raw);
    // 全局搜索：path 为空 + 有搜索词 → 全库跨目录搜索（显示所在路径列）
    let has_global_search = raw.trim().is_empty()
        && params
            .get("search")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let files = query_files(
        &pool,
        &session.username,
        &raw,
        params.get("search").map(|s| s.as_str()),
        params.get("sort_by").map(|s| s.as_str()),
    )
    .await;
    AppTemplate(FileTableFragment {
        files,
        current_path: path,
        has_global_search,
        search: params.get("search").cloned().unwrap_or_default(),
    })
}

/// GET /drive/breadcrumbs
#[tracing::instrument(skip_all)]
pub async fn drive_breadcrumbs_fragment(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let mut parts = vec![BreadcrumbPart {
        name: "~".to_string(),
        path: "/".to_string(),
    }];
    if path != "/" {
        let mut acc = String::new();
        for seg in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
            acc.push('/');
            acc.push_str(seg);
            parts.push(BreadcrumbPart {
                name: seg.to_string(),
                path: acc.clone(),
            });
        }
    }
    AppTemplate(BreadcrumbsFragment { parts })
}

/// GET /drive/upload-form
pub async fn drive_upload_form(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let path = normalize_display_path(&raw);
    AppTemplate(UploadFormFragment { current_path: path })
}

/// GET /drive/new-folder-form
pub async fn drive_new_folder_form() -> impl IntoResponse {
    AppTemplate(NewFolderFormFragment {})
}

/// GET /drive/quota
#[tracing::instrument(skip_all)]
pub async fn drive_quota_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let row = sqlx::query("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(&session.username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None);
    let (used, total) = row.map_or((0, 0), |r| (r.get::<i64, _>(0), r.get::<i64, _>(1)));
    let percent = if total > 0 {
        ((used as f64 / total as f64) * 100.0).min(100.0) as u32
    } else {
        0
    };
    AppTemplate(QuotaFragment {
        used_mb: used,
        total_mb: total,
        percent,
    })
}

/// POST /drive/create-folder
#[tracing::instrument(skip_all)]
pub async fn drive_create_folder(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = form
        .get("name")
        .cloned()
        .unwrap_or_default()
        .trim()
        .to_string();
    let raw = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let display = normalize_display_path(&raw);
    let parent = user_dir_path(Some(raw));
    if name.is_empty() {
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            display.clone(),
            "文件夹名不能为空",
        )
        .await;
    }
    // 字符串级纵深防御校验（内核级 openat2 兜底）；返回值不再用于物理操作
    let _target = match create_folder_common(
        &session.username,
        &parent,
        &name,
        std::path::Path::new(crate::constants::UPLOADS_DIR),
    ) {
        Ok(p) => p,
        Err(_) => {
            return fallback_file_list_with_error(
                pool.clone(),
                session.username.clone(),
                display.clone(),
                "文件夹名包含非法字符",
            )
            .await;
        }
    };
    // M11 一致性：INSERT 先行——UNIQUE 冲突即同名（绝不删除他人目录），成功后再建物理目录
    match sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
        .bind(&session.username)
        .bind(&name)
        .bind(&parent)
        .execute(&pool)
        .await
    {
        Ok(_) => {}
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation()) =>
        {
            return fallback_file_list_with_error(
                pool.clone(),
                session.username.clone(),
                display.clone(),
                "同名文件夹已存在",
            )
            .await;
        }
        Err(e) => {
            tracing::error!("[Drive] 文件夹行登记失败: {}", e);
            return fallback_file_list_with_error(
                pool.clone(),
                session.username.clone(),
                display.clone(),
                "创建文件夹失败",
            )
            .await;
        }
    }
    let rel = user_file_path(&session.username, &parent, &name);
    if let Err(e) = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .and_then(|sb| sb.create_dir_all(&rel))
    {
        tracing::error!("[Drive] 创建目录失败: {}", e);
        let _ = sqlx::query(
            "DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ? AND is_dir = 1",
        )
        .bind(&session.username)
        .bind(&name)
        .bind(&parent)
        .execute(&pool)
        .await;
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            display.clone(),
            "创建文件夹失败",
        )
        .await;
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "create_folder",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), display.clone()).await
}

/// POST /drive/delete
#[tracing::instrument(skip_all)]
pub async fn drive_delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = form.get("name").cloned().unwrap_or_default();
    let current_path = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let parent = user_dir_path(Some(current_path.clone()));
    if name.is_empty() {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone())
            .await;
    }
    if let Err(e) = delete_to_trash(&pool, &session.username, &parent, &name).await {
        tracing::error!("[Drive] 删除失败: {}", e);
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            current_path.clone(),
            "删除失败，请稍后重试",
        )
        .await;
    }
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(
        &pool,
        &session.username,
        "delete",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

/// GET /drive/rename-form
pub async fn drive_rename_form(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    AppTemplate(RenameFormFragment {
        current_path: params
            .get("current_path")
            .cloned()
            .unwrap_or_else(|| "/".to_string()),
        old_name: params.get("name").cloned().unwrap_or_default(),
    })
}

/// POST /drive/rename
#[tracing::instrument(skip_all)]
pub async fn drive_rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let old_name = form.get("old_name").cloned().unwrap_or_default();
    let new_name = form
        .get("new_name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let parent = current_path.trim_start_matches('/');
    if old_name.is_empty() || new_name.is_empty() || old_name == new_name {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone())
            .await;
    }
    if let Err(e) = rename_core(&pool, &session.username, parent, &old_name, &new_name).await {
        tracing::error!("[Drive] 重命名失败: {}", e);
        let msg = match e {
            AppError::Conflict(m) | AppError::BadRequest(m) => m,
            _ => "重命名失败，请稍后重试".to_string(),
        };
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            current_path.clone(),
            &msg,
        )
        .await;
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "rename",
        Some(&logical_path(parent, &old_name)),
        Some(&format!("-> {}", new_name)),
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

/// GET /drive/move-form
#[tracing::instrument(skip_all)]
pub async fn drive_move_form(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = params
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let name = params.get("name").cloned().unwrap_or_default();
    #[derive(sqlx::FromRow)]
    struct D {
        parent_path: String,
        name: String,
    }
    let rows: Vec<D> = sqlx::query_as("SELECT parent_path, name FROM files WHERE username = ? AND is_dir = 1 ORDER BY parent_path, name")
        .bind(&session.username).fetch_all(&pool).await.unwrap_or_default();
    let mut dirs: Vec<String> = rows
        .iter()
        .map(|r| logical_path(&r.parent_path, &r.name))
        .collect();
    let source = current_path.trim_start_matches('/');
    let source_full = logical_path(source, &name);
    let prefix = format!("{}/", source_full);
    dirs.retain(|p| {
        let p = p.trim_start_matches('/');
        p != source_full && !p.starts_with(&prefix)
    });
    dirs.sort();
    AppTemplate(MoveFormFragment {
        current_path,
        name,
        dirs,
    })
}

/// POST /drive/move
#[tracing::instrument(skip_all)]
pub async fn drive_move_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let name = form.get("name").cloned().unwrap_or_default();
    let target_dir = form.get("target_dir").cloned().unwrap_or_default();
    let src = current_path.trim_start_matches('/');
    let dst = target_dir.trim_start_matches('/');
    if name.is_empty() || src == dst {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone())
            .await;
    }
    if let Err(e) = move_core(&pool, &session.username, src, dst, &name).await {
        tracing::error!("[Drive] 移动失败: {}", e);
        let msg = match e {
            AppError::Conflict(m) | AppError::BadRequest(m) => m,
            _ => "移动失败，请稍后重试".to_string(),
        };
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            current_path.clone(),
            &msg,
        )
        .await;
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "move",
        Some(&logical_path(src, &name)),
        Some(&format!("-> {}", dst)),
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

// ====== 文件预览 ======

const MAX_PREVIEW_TEXT_SIZE: u64 = 1024 * 1024;

/// GET /drive/preview
#[tracing::instrument(skip_all)]
pub async fn drive_preview(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> AppResult<AppTemplate<PreviewFragment>> {
    let current_path = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let name = params.get("name").cloned().unwrap_or_default();
    let parent = current_path.trim_start_matches('/');
    let file_path = logical_path(parent, &name);
    // 字符串级纵深防御（内核级 openat2 兜底）
    let _full_path = safe_join_sandbox(
        std::path::Path::new(crate::constants::UPLOADS_DIR),
        &user_file_path(&session.username, parent, &name),
    )?;
    let rel = user_file_path(&session.username, parent, &name);
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;

    let file_size = match sb.metadata(&rel) {
        Ok(meta) => bytes_to_mb_string(meta.len()),
        Err(_) => String::from("未知"),
    };

    let mime = normalize_preview_mime(&name, mime_guess::from_path(&name).first_or_octet_stream());
    let mime_str = mime.to_string();
    let is_image = mime.type_().as_str() == "image";
    let is_video = mime.type_().as_str() == "video";
    let is_audio = mime.type_().as_str() == "audio";
    let is_pdf = mime.subtype().as_str() == "pdf" || name.to_lowercase().ends_with(".pdf");
    let is_markdown = ["md", "markdown", "mdx"]
        .iter()
        .any(|e| name.to_lowercase().ends_with(&format!(".{}", e)));

    let text_mimes = [
        "text",
        "application/json",
        "application/xml",
        "application/javascript",
        "application/x-sh",
        "application/x-python",
    ];
    let text_exts = [
        "txt",
        "md",
        "rs",
        "py",
        "js",
        "ts",
        "html",
        "css",
        "json",
        "xml",
        "yaml",
        "yml",
        "toml",
        "ini",
        "cfg",
        "conf",
        "sh",
        "bash",
        "zsh",
        "sql",
        "log",
        "env",
        "c",
        "cpp",
        "h",
        "hpp",
        "java",
        "go",
        "rb",
        "php",
        "swift",
        "kt",
        "scala",
        "r",
        "lua",
        "vim",
        "dockerfile",
        "makefile",
        "editorconfig",
        "gitignore",
    ];
    let is_text = text_mimes.iter().any(|t| mime_str.starts_with(t))
        || text_exts.iter().any(|e| {
            name.to_lowercase().ends_with(&format!(".{}", e)) || name.to_lowercase() == *e
        });

    let content = if is_text || is_markdown {
        sb.read_to_string(&rel)
            .map(|s| {
                if s.len() as u64 > MAX_PREVIEW_TEXT_SIZE {
                    format!(
                        "[文件过大，仅显示前 1 MB]\n\n{}",
                        s.chars()
                            .take(MAX_PREVIEW_TEXT_SIZE as usize)
                            .collect::<String>()
                    )
                } else {
                    s
                }
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Markdown 原文 JSON 编码后嵌入 script 数据块（< → < 防 </script> 逃逸；前端 marked+DOMPurify 渲染）
    let markdown_json = if is_markdown {
        serde_json::to_string(&content)
            .unwrap_or_default()
            .replace('<', "\\u003c")
    } else {
        String::new()
    };
    // 图片画廊：同目录相邻文件（按文件名排序）
    let (prev, next) = if is_image {
        gallery_neighbors(&pool, &session.username, parent, &name).await
    } else {
        (None, None)
    };
    let (prev_path, prev_name) = match prev {
        Some(p) => (p.path, p.name),
        None => (String::new(), String::new()),
    };
    let (next_path, next_name) = match next {
        Some(p) => (p.path, p.name),
        None => (String::new(), String::new()),
    };

    // 媒体类型签发短时效目录限定令牌（img/video/audio/iframe 无法带 Cookie/Authorization）
    let media_token = if is_image || is_video || is_audio || is_pdf {
        crate::handlers::utils::issue_media_token(&pool, &session.username, parent).await
    } else {
        String::new()
    };

    Ok(AppTemplate(PreviewFragment {
        file_name: name,
        file_path,
        file_size,
        mime_type: mime_str,
        is_image,
        is_video,
        is_audio,
        is_pdf,
        is_text,
        content,
        username: session.username,
        is_markdown,
        markdown_json,
        prev_path,
        prev_name,
        next_path,
        next_name,
        media_token,
    }))
}

/// 同目录相邻文件（忽略目录，按文件名 NOCASE 排序）——图片画廊翻页
async fn gallery_neighbors(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent_path: &str,
    name: &str,
) -> (Option<PreviewNav>, Option<PreviewNav>) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, parent_path FROM files WHERE username = ? AND parent_path = ? AND is_dir = 0 \
         ORDER BY name COLLATE NOCASE",
    )
    .bind(username)
    .bind(parent_path)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let Some(i) = rows.iter().position(|(n, _)| n == name) else {
        return (None, None);
    };
    let prev = rows
        .get(i.wrapping_sub(1))
        .filter(|_| i > 0)
        .map(|(n, p)| PreviewNav {
            path: p.clone(),
            name: n.clone(),
        });
    let next = rows.get(i + 1).map(|(n, p)| PreviewNav {
        path: p.clone(),
        name: n.clone(),
    });
    (prev, next)
}
