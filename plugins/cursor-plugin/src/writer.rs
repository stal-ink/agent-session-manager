use std::fs;

use anyhow::Result;
use serde_json::{json, Value};

use agent_core::ir::{Block, Role, Session};
use agent_core::sqlite::open_rw;
use agent_core::text::{push_text, tool_status, truncate};
use agent_core::time::ms_to_iso;

pub fn inject(paths: &super::Paths, session: &Session, target_cwd: &str) -> Result<String> {
    inject_with(paths, session, target_cwd, None)
}

pub fn overwrite(
    paths: &super::Paths,
    session: &Session,
    target_cwd: &str,
    target_id: &str,
) -> Result<String> {
    inject_with(paths, session, target_cwd, Some(target_id))
}

fn inject_with(
    paths: &super::Paths,
    session: &Session,
    target_cwd: &str,
    existing: Option<&str>,
) -> Result<String> {
    let target_cwd = if target_cwd.trim().is_empty() {
        session.cwd.as_str()
    } else {
        target_cwd
    };
    let id = existing
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created = if session.created_ms > 0 {
        session.created_ms
    } else {
        agent_core::ir::now_ms()
    };
    let updated = agent_core::ir::now_ms();
    let title = if session.title.trim().is_empty() {
        "转接会话".to_string()
    } else {
        session.title.trim().to_string()
    };

    let (workspace_id, workspace_uri) = workspace_for(paths, target_cwd)?;
    let workspace_identifier = match workspace_uri {
        Some(uri) => json!({ "id": workspace_id, "uri": uri }),
        None => json!({ "id": workspace_id }),
    };

    let mut bubbles: Vec<Value> = Vec::new();
    let mut headers: Vec<Value> = Vec::new();
    for (index, message) in session.messages.iter().enumerate() {
        let base_ms = if message.time_ms > 0 {
            message.time_ms
        } else {
            created + index as i64
        };
        for entry in message_bubbles(message.role, &message.blocks, base_ms) {
            headers.push(entry.header);
            bubbles.push(entry.bubble);
        }
    }

    let mut composer = json!({
        "_v": 18,
        "composerId": id.clone(),
        "createdAt": created,
        "lastUpdatedAt": updated,
        "conversationCheckpointLastUpdatedAt": updated,
        "name": title.clone(),
        "subtitle": truncate(&title, 160),
        "text": "",
        "richText": rich_text(""),
        "status": "completed",
        "unifiedMode": "agent",
        "forceMode": "edit",
        "isAgentic": true,
        "isDraft": false,
        "isNAL": true,
        "hasLoaded": true,
        "hasUnreadMessages": false,
        "isFileListExpanded": false,
        "isQueueExpanded": true,
        "isBestOfNParent": false,
        "isBestOfNSubcomposer": false,
        "isProject": false,
        "isSpec": false,
        "isSpecSubagentDone": false,
        "isContinuationInProgress": false,
        "isCreatingWorktree": false,
        "isApplyingWorktree": false,
        "isUndoingWorktree": false,
        "applied": false,
        "pendingCreateWorktree": false,
        "worktreeStartedReadOnly": false,
        "planModeSuggestionUsed": false,
        "debugModeSuggestionUsed": false,
        "gitHubPromptDismissed": false,
        "browserChipManuallyDisabled": false,
        "browserChipManuallyEnabled": false,
        "canvasPillCollapsed": false,
        "activeTabsShouldBeReactive": true,
        "hasChangedContext": false,
        "isReadingLongFile": false,
        "stopHookLoopCount": 0,
        "addedFiles": 0,
        "removedFiles": 0,
        "totalLinesAdded": 0,
        "totalLinesRemoved": 0,
        "contextUsagePercent": 0,
        "contextTokensUsed": 0,
        "contextTokenLimit": 0,
        "conversationState": "~",
        "conversationMap": {},
        "generatingBubbleIds": [],
        "latestChatGenerationUUID": uuid::Uuid::new_v4().to_string(),
        "subComposerIds": [],
        "subagentComposerIds": [],
        "todos": [],
        "queueItems": [],
        "trackedGitRepos": [],
        "usageData": {},
        "modelConfig": {
            "modelName": "default",
            "maxMode": false,
            "selectedModels": [{ "modelId": "default", "parameters": [] }]
        },
        "capabilities": [
            { "type": 15, "data": { "bubbleDataMap": "{}" } },
            { "type": 19, "data": {} },
            { "type": 33, "data": {} },
            { "type": 32, "data": {} },
            { "type": 23, "data": {} },
            { "type": 16, "data": {} },
            { "type": 24, "data": {} },
            { "type": 21, "data": {} }
        ],
        "capabilityContexts": [],
        "context": empty_context(),
        "codeBlockData": {},
        "originalFileStates": {},
        "newlyCreatedFiles": [],
        "newlyCreatedFolders": [],
        "allAttachedFileCodeChunksUris": [],
        "blobEncryptionKey": random_key(),
        "speculativeSummarizationEncryptionKey": random_key(),
        "workspaceIdentifier": workspace_identifier,
        "fullConversationHeadersOnly": headers,
    });
    if workspace_identifier.get("uri").is_none() {
        composer["workspaceIdentifier"] = json!({ "id": workspace_id });
    }

    let head = {
        let mut head = json!({
            "type": "head",
            "composerId": id.clone(),
            "name": title.clone(),
            "createdAt": created,
            "lastUpdatedAt": updated,
            "conversationCheckpointLastUpdatedAt": updated,
            "unifiedMode": "agent",
            "forceMode": "edit",
            "hasUnreadMessages": false,
            "totalLinesAdded": 0,
            "totalLinesRemoved": 0,
            "filesChangedCount": 0,
            "subtitle": truncate(&title, 160),
            "hasBlockingPendingActions": false,
            "hasPendingPlan": false,
            "isDraft": false,
            "isWorktree": false,
            "worktreeStartedReadOnly": false,
            "isSpec": false,
            "isProject": false,
            "isBestOfNSubcomposer": false,
            "numSubComposers": 0,
            "referencedPlans": [],
            "branches": [],
            "trackedGitRepos": [],
        });
        head["workspaceIdentifier"] = composer["workspaceIdentifier"].clone();
        head
    };

    if !paths.state_db.is_file() {
        anyhow::bail!("Cursor 会话库不存在：{}", paths.state_db.display());
    }
    let db = open_rw(&paths.state_db)?;
    let has_headers: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='composerHeaders'",
        [],
        |r| r.get(0),
    )?;
    if has_headers == 0 {
        anyhow::bail!("{} 不是 Cursor 会话库", paths.state_db.display());
    }

    let tx = db.unchecked_transaction()?;
    if existing.is_some() {
        tx.execute(
            "DELETE FROM cursorDiskKV WHERE key = ?1",
            [format!("composerData:{id}")],
        )?;
        tx.execute(
            "DELETE FROM cursorDiskKV WHERE key LIKE ?1",
            [format!("bubbleId:{id}:%")],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO composerHeaders
         (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent, recency, checkpointAt, value, subagentTypeName)
         VALUES (?1, ?2, ?3, ?4, 0, 0, ?4, NULL, ?5, '')",
        rusqlite::params![id, workspace_id, created, updated, head.to_string()],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
        rusqlite::params![format!("composerData:{id}"), composer.to_string()],
    )?;
    for bubble in &bubbles {
        let bubble_id = bubble["bubbleId"].as_str().unwrap_or_default();
        tx.execute(
            "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("bubbleId:{id}:{bubble_id}"), bubble.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

struct Entry {
    bubble: Value,
    header: Value,
}

fn message_bubbles(role: Role, blocks: &[Block], base_ms: i64) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tools: Vec<(String, String, Value, Option<String>, String)> = Vec::new();

    for block in blocks {
        match block {
            Block::Text(value) => push_text(&mut text, value),
            Block::Thinking(value) => push_text(&mut thinking, value),
            Block::ToolCall {
                id,
                name,
                input,
                output,
                status,
            } => tools.push((
                id.clone(),
                name.clone(),
                input.clone(),
                output.clone(),
                status.clone(),
            )),
            Block::Image { reference } => push_text(&mut text, &format!("[图片] {reference}")),
            Block::File { path } => push_text(&mut text, &format!("[文件] {path}")),
        }
    }

    let mut offset = 0i64;
    if role == Role::Assistant {
        if !thinking.trim().is_empty() {
            entries.push(thinking_bubble(&thinking, base_ms + offset));
            offset += 1;
        }
        if !text.trim().is_empty() {
            entries.push(text_bubble(2, &text, base_ms + offset));
            offset += 1;
        }
        for (index, (tool_id, name, input, output, status)) in tools.into_iter().enumerate() {
            entries.push(tool_bubble(
                &tool_id,
                &name,
                &input,
                output.as_deref(),
                &status,
                base_ms + offset,
                index,
            ));
            offset += 1;
        }
    } else {
        if !thinking.trim().is_empty() {
            push_text(&mut text, &thinking);
        }
        for (_, name, input, output, _) in tools {
            let args = input.to_string();
            let mut line = format!("[工具调用] {name} {args}");
            if let Some(output) = output {
                line.push_str(&format!("\n[结果] {output}"));
            }
            push_text(&mut text, &line);
        }
        if !text.trim().is_empty() {
            entries.push(text_bubble(1, &text, base_ms));
        }
    }
    entries
}

fn text_bubble(kind: i64, text: &str, ms: i64) -> Entry {
    let id = uuid::Uuid::new_v4().to_string();
    let created = ms_to_iso(ms);
    let mut bubble = base_bubble(&id, kind, &created);
    bubble["text"] = json!(text);
    if kind == 1 {
        bubble["richText"] = json!(rich_text(text));
        bubble["context"] = empty_context();
        bubble["modelInfo"] = json!({ "modelName": "default" });
        bubble["requestId"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let mut grouping = json!({
        "isRenderable": true,
        "hasText": true,
        "isShortPlainText": !text.contains('\n'),
        "toolDisplayComputed": true
    });
    if kind == 2 {
        grouping["isKeptFinalAiVisibleOutsideWorkedForGroup"] = json!(true);
    }
    Entry {
        header: header(&id, kind, &created, grouping),
        bubble,
    }
}

fn thinking_bubble(text: &str, ms: i64) -> Entry {
    let id = uuid::Uuid::new_v4().to_string();
    let created = ms_to_iso(ms);
    let mut bubble = base_bubble(&id, 2, &created);
    bubble["thinking"] = json!({ "text": text, "signature": "" });
    bubble["thinkingDurationMs"] = json!(0);
    bubble["thinkingStyle"] = json!(1);
    bubble["capabilityType"] = json!(30);
    Entry {
        header: header(
            &id,
            2,
            &created,
            json!({
                "isRenderable": true,
                "capabilityType": 30,
                "hasThinking": true,
                "thinkingDurationMs": 0,
                "toolDisplayComputed": true
            }),
        ),
        bubble,
    }
}

fn tool_bubble(
    tool_call_id: &str,
    name: &str,
    input: &Value,
    output: Option<&str>,
    status: &str,
    ms: i64,
    index: usize,
) -> Entry {
    let id = uuid::Uuid::new_v4().to_string();
    let created = ms_to_iso(ms);
    let call_id = if tool_call_id.is_empty() {
        format!("call_{}", uuid::Uuid::new_v4())
    } else {
        tool_call_id.to_string()
    };
    let args = input.to_string();
    let status = tool_status(status);
    let result = output.unwrap_or_default();
    let mut bubble = base_bubble(&id, 2, &created);
    bubble["capabilityType"] = json!(15);
    bubble["toolFormerData"] = json!({
        "toolCallId": call_id.clone(),
        "toolIndex": index,
        "modelCallId": call_id.clone(),
        "status": status,
        "name": name,
        "rawArgs": args.clone(),
        "params": args,
        "result": result
    });
    let mut grouping = json!({
        "isRenderable": true,
        "capabilityType": 15,
        "toolFormerStatus": status,
        "toolCallId": call_id,
        "isToolGroupable": true,
        "toolDisplayComputed": true
    });
    if let Some(path) = display_path(input) {
        grouping["toolDisplayPath"] = json!(path);
    }
    Entry {
        header: header(&id, 2, &created, grouping),
        bubble,
    }
}

fn display_path(input: &Value) -> Option<String> {
    for key in [
        "path",
        "file_path",
        "filePath",
        "targetFile",
        "target_file",
        "targetDirectory",
        "target_directory",
        "absPath",
    ] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn header(bubble_id: &str, kind: i64, created: &str, grouping: Value) -> Value {
    json!({
        "bubbleId": bubble_id,
        "type": kind,
        "grouping": grouping,
        "createdAt": created
    })
}

fn base_bubble(id: &str, kind: i64, created: &str) -> Value {
    json!({
        "_v": 3,
        "bubbleId": id,
        "type": kind,
        "text": "",
        "createdAt": created,
        "conversationState": "~",
        "unifiedMode": 2,
        "isAgentic": false,
        "isRefunded": false,
        "isPlanExecution": false,
        "tokenCount": { "inputTokens": 0, "outputTokens": 0 },
        "requestId": "",
        "capabilities": [],
        "capabilityContexts": [],
        "contextPieces": [],
        "codeBlocks": [],
        "codebaseContextChunks": [],
        "commits": [],
        "consoleLogs": [],
        "cursorCommands": [],
        "cursorRules": [],
        "deletedFiles": [],
        "diffHistories": [],
        "diffsForCompressingFiles": [],
        "diffsSinceLastApply": [],
        "docsReferences": [],
        "documentationSelections": [],
        "editTrailContexts": [],
        "externalLinks": [],
        "fileDiffTrajectories": [],
        "gitDiffs": [],
        "humanChanges": [],
        "images": [],
        "interpreterResults": [],
        "knowledgeItems": [],
        "lints": [],
        "mcpDescriptors": [],
        "multiFileLinterErrors": [],
        "notepads": [],
        "pastChats": [],
        "pastChatsExplicitlySet": false,
        "projectLayouts": [],
        "pullRequests": [],
        "recentLocationsHistory": [],
        "recentlyViewedFiles": [],
        "relevantFiles": [],
        "suggestedCodeBlocks": [],
        "summarizedComposers": [],
        "supportedTools": [],
        "todos": [],
        "toolResults": [],
        "uiElementPicked": [],
        "userResponsesToSuggestedCodeBlocks": [],
        "webReferences": [],
        "workspaceUris": [],
        "attachedCodeChunks": [],
        "attachedFileCodeChunksMetadataOnly": [],
        "attachedFolders": [],
        "attachedFoldersListDirResults": [],
        "attachedFoldersNew": [],
        "attachedHumanChanges": false,
        "aiWebSearchResults": [],
        "allThinkingBlocks": [],
        "approximateLintErrors": [],
        "assistantSuggestedDiffs": [],
        "existedPreviousTerminalCommand": false,
        "existedSubsequentTerminalCommand": false,
        "cursorCommandsExplicitlySet": false
    })
}

fn empty_context() -> Value {
    json!({
        "composers": [],
        "selectedCommits": [],
        "selectedPullRequests": [],
        "selectedImages": [],
        "selectedDocuments": [],
        "selectedVideos": [],
        "folderSelections": [],
        "fileSelections": [],
        "terminalFiles": [],
        "selections": [],
        "terminalSelections": [],
        "selectedDocs": [],
        "externalLinks": [],
        "cursorRules": [],
        "cursorCommands": [],
        "gitPRDiffSelections": [],
        "subagentSelections": [],
        "browserSelections": [],
        "extraContext": [],
        "mentions": {
            "composers": {},
            "selectedCommits": {},
            "selectedPullRequests": {},
            "gitDiff": [],
            "gitDiffFromBranchToMain": [],
            "selectedImages": {},
            "selectedDocuments": {},
            "selectedVideos": {},
            "folderSelections": {},
            "fileSelections": {},
            "terminalFiles": {},
            "selections": {},
            "terminalSelections": {},
            "selectedDocs": {},
            "externalLinks": {},
            "diffHistory": [],
            "cursorRules": {},
            "cursorCommands": {},
            "uiElementSelections": [],
            "consoleLogs": [],
            "ideEditorsState": [],
            "gitPRDiffSelections": {},
            "subagentSelections": {},
            "browserSelections": {}
        }
    })
}

fn rich_text(text: &str) -> String {
    let paragraphs: Vec<Value> = if text.is_empty() {
        vec![paragraph(None)]
    } else {
        text.split('\n').map(|line| paragraph(Some(line))).collect()
    };
    json!({
        "root": {
            "children": paragraphs,
            "direction": "ltr",
            "format": "",
            "indent": 0,
            "type": "root",
            "version": 1
        }
    })
    .to_string()
}

fn paragraph(line: Option<&str>) -> Value {
    let children: Vec<Value> = match line {
        Some(text) if !text.is_empty() => vec![json!({
            "detail": 0,
            "format": 0,
            "mode": "normal",
            "style": "",
            "text": text,
            "type": "text",
            "version": 1
        })],
        _ => vec![],
    };
    json!({
        "children": children,
        "direction": "ltr",
        "format": "",
        "indent": 0,
        "type": "paragraph",
        "version": 1
    })
}

fn random_key() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn workspace_for(paths: &super::Paths, cwd: &str) -> Result<(String, Option<Value>)> {
    if cwd.trim().is_empty() {
        return Ok(("empty-window".to_string(), None));
    }
    let target = agent_core::path::key(cwd);
    if let Ok(entries) = fs::read_dir(&paths.workspace_storage) {
        for entry in entries.flatten() {
            let file = entry.path().join("workspace.json");
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if let Some(folder) = value["folder"].as_str() {
                if agent_core::path::key(&super::uri_to_path(folder)) == target {
                    return Ok((
                        entry.file_name().to_string_lossy().to_string(),
                        Some(uri_object(cwd)),
                    ));
                }
            }
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let dir = paths.workspace_storage.join(&id);
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("workspace.json"),
        serde_json::to_string_pretty(&json!({ "folder": path_to_uri(cwd) }))?,
    )?;
    Ok((id, Some(uri_object(cwd))))
}

fn uri_object(path: &str) -> Value {
    let mut decoded = agent_core::path::lower_drive(&path.replace('\\', "/"));
    if !decoded.starts_with('/') {
        decoded = format!("/{decoded}");
    }
    json!({
        "$mid": 1,
        "fsPath": fs_path(path),
        "_sep": 1,
        "external": path_to_uri(path),
        "path": decoded,
        "scheme": "file"
    })
}

fn fs_path(path: &str) -> String {
    agent_core::path::lower_drive(&agent_core::path::normalize(path))
}

fn path_to_uri(path: &str) -> String {
    let mut normalized = agent_core::path::lower_drive(&path.replace('\\', "/"));
    if !normalized.starts_with('/') {
        normalized = format!("/{normalized}");
    }
    let mut out = String::from("file://");
    for ch in normalized.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' => out.push(ch),
            ':' => out.push_str("%3A"),
            _ => {
                let mut buffer = [0u8; 4];
                for byte in ch.encode_utf8(&mut buffer).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}
