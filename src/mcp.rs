use std::fs;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::parser::{self, ContextBundle, ModuleEdit, ModuleNode};

/// Deserializador flexible que permite recibir tanto un `"id"` suelto como un arreglo `["id1", "id2"]`.
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Multiple(Vec<String>),
    }

    match Option::<StringOrVec>::deserialize(deserializer)? {
        Some(StringOrVec::Single(s)) => {
            if s.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![s])
            }
        }
        Some(StringOrVec::Multiple(v)) => Ok(v),
        None => Ok(Vec::new()),
    }
}

/// Petición estándar JSON-RPC 2.0 recibida por `stdio`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Respuesta estándar JSON-RPC 2.0 enviada por `stdio`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// Objeto de error estándar JSON-RPC 2.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Parámetros de invocación de una tool MCP (`tools/call`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

/// Argumentos para la tool MCP `get_document_index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetDocumentIndexArgs {
    pub file_path: String,
}

/// Respuesta estructurada de la tool MCP `get_document_index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentIndexResponse {
    pub file_path: String,
    pub modules: Vec<ModuleNode>,
}

/// Argumentos para la tool MCP `prepare_module_edit` (soporta lote `target_ids` y `target_id` individual).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareModuleEditArgs {
    pub file_path: String,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub target_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub guide_ids: Vec<String>,
}

impl PrepareModuleEditArgs {
    /// Fusiona `target_ids` y `target_id` preservando el orden y evitando duplicados.
    pub fn resolved_target_ids(&self) -> Vec<String> {
        let mut ids = self.target_ids.clone();
        if let Some(single) = &self.target_id
            && !single.is_empty()
            && !ids.contains(single)
        {
            ids.push(single.clone());
        }
        ids
    }
}

/// Argumentos para la tool MCP `apply_module_edit` (soporta lote `edits` y modo legacy de un solo elemento).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyModuleEditArgs {
    pub file_path: String,
    #[serde(default)]
    pub edits: Vec<ModuleEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(
        default,
        alias = "new_content",
        skip_serializing_if = "Option::is_none"
    )]
    pub new_markdown_content: Option<String>,
}

impl ApplyModuleEditArgs {
    /// Devuelve la lista efectiva de ediciones combinando `edits` y los campos opcionales de raíz.
    pub fn resolved_edits(&self) -> Vec<ModuleEdit> {
        let mut edits = self.edits.clone();
        if let Some(target_id) = &self.target_id
            && !target_id.is_empty()
        {
            let new_content = self.new_markdown_content.clone().unwrap_or_default();
            edits.push(ModuleEdit {
                target_id: target_id.clone(),
                new_content,
            });
        }
        edits
    }
}

/// Confirmación devuelta por la tool MCP `apply_module_edit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyModuleEditResponse {
    pub file_path: String,
    pub updated_targets: Vec<String>,
    pub updated: bool,
    pub bytes_written: usize,
}

/// Ejecuta la tool `get_document_index`.
pub fn handle_get_document_index(args: GetDocumentIndexArgs) -> Result<DocumentIndexResponse> {
    let markdown = fs::read_to_string(&args.file_path)
        .with_context(|| format!("Error al leer el archivo '{}'", args.file_path))?;
    let modules = parser::get_document_tree(&markdown);
    Ok(DocumentIndexResponse {
        file_path: args.file_path,
        modules,
    })
}

/// Ejecuta la tool `prepare_module_edit`.
pub fn handle_prepare_module_edit(args: PrepareModuleEditArgs) -> Result<ContextBundle> {
    let markdown = fs::read_to_string(&args.file_path)
        .with_context(|| format!("Error al leer el archivo '{}'", args.file_path))?;
    let target_ids = args.resolved_target_ids();
    parser::extract_context_bundle(&markdown, &target_ids, &args.guide_ids)
}

/// Ejecuta la tool `apply_module_edit`.
pub fn handle_apply_module_edit(args: ApplyModuleEditArgs) -> Result<ApplyModuleEditResponse> {
    let markdown = fs::read_to_string(&args.file_path)
        .with_context(|| format!("Error al leer el archivo '{}'", args.file_path))?;
    let edits = args.resolved_edits();
    let updated_targets: Vec<String> = edits.iter().map(|e| e.target_id.clone()).collect();
    let updated_markdown = parser::replace_multiple_modules(&markdown, &edits)?;
    let bytes_written = updated_markdown.len();

    fs::write(&args.file_path, &updated_markdown)
        .with_context(|| format!("Error al escribir en el archivo '{}'", args.file_path))?;

    Ok(ApplyModuleEditResponse {
        file_path: args.file_path,
        updated_targets,
        updated: true,
        bytes_written,
    })
}

/// Devuelve el catálogo de tools MCP expuestas por el servidor junto con sus `annotations`.
fn get_tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "get_document_index",
                "description": "ALWAYS CALL THIS FIRST before reading or editing any Markdown (.md) file. Preferred over read_file. Parses the document without loading full text into context and returns a lightweight hierarchical semantic tree of modules (#, ##) and submodules (###). Each node provides unique hierarchical composite IDs ('parent/child' for submodules to avoid collisions) and a concise summary (max 120 chars) to navigate the document structure efficiently.",
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "required": ["file_path"],
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path to the Markdown file to index."
                        }
                    }
                }
            },
            {
                "name": "prepare_module_edit",
                "description": "Extracts the current content of target modules/submodules ('target_ids') and optional read-only reference context ('guide_ids') in a single call. Use 'target_ids' to prepare batch edits. Use 'guide_ids' whenever you need read-only context (e.g. referencing architectural specs or data models to implement code in other files without modifying the Markdown document itself). Avoids reading unneeded document parts.",
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "required": ["file_path"],
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path to the Markdown file."
                        },
                        "target_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Array of hierarchical module IDs (e.g. '2-database') or submodule IDs ('2-database/payments-table') to edit in batch."
                        },
                        "target_id": {
                            "type": "string",
                            "description": "Optional (backward compatibility): Single module or submodule ID to edit."
                        },
                        "guide_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "default": [],
                            "description": "Optional array of module or submodule IDs to load as read-only reference guides."
                        }
                    }
                }
            },
            {
                "name": "apply_module_edit",
                "description": "Surgically patches one or multiple modules/submodules in disk using exact byte ranges in a single atomic bottom-up batch operation ('edits'). Guarantees that unmodified sections and byte offsets remain intact without rewriting the whole file. Setting 'new_markdown_content' to empty string (\"\") cleanly deletes the target section without leaving orphan triple newlines.",
                "annotations": {
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": true
                },
                "inputSchema": {
                    "type": "object",
                    "required": ["file_path"],
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path to the Markdown file."
                        },
                        "edits": {
                            "type": "array",
                            "description": "Batch of module edits to apply atomically.",
                            "items": {
                                "type": "object",
                                "required": ["target_id", "new_markdown_content"],
                                "properties": {
                                    "target_id": {
                                        "type": "string",
                                        "description": "Hierarchical ID of the target module or submodule."
                                    },
                                    "new_markdown_content": {
                                        "type": "string",
                                        "description": "New complete Markdown content for the section (or empty string '' to delete it)."
                                    }
                                }
                            }
                        },
                        "target_id": {
                            "type": "string",
                            "description": "Optional (backward compatibility): Hierarchical ID of a single module or submodule to overwrite."
                        },
                        "new_markdown_content": {
                            "type": "string",
                            "description": "Optional (backward compatibility): New Markdown content when target_id is passed at root."
                        }
                    }
                }
            }
        ]
    })
}

/// Empaqueta una respuesta exitosa de herramienta MCP en el formato de contenido textual JSON.
fn wrap_tool_ok<T: Serialize>(payload: &T) -> Result<Value> {
    let text = serde_json::to_string_pretty(payload)?;
    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "isError": false
    }))
}

/// Empaqueta un error de ejecución de herramienta MCP.
fn wrap_tool_err(err: anyhow::Error) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": format!("{err:#}")
            }
        ],
        "isError": true
    })
}

/// Despacha una llamada `tools/call` hacia el manejador correspondiente.
fn dispatch_tool_call(params: Option<Value>) -> std::result::Result<Value, JsonRpcError> {
    let raw_params = params.ok_or_else(|| JsonRpcError {
        code: -32602,
        message: "Faltan parámetros en tools/call".to_string(),
        data: None,
    })?;

    let call: ToolCallParams = serde_json::from_value(raw_params).map_err(|e| JsonRpcError {
        code: -32602,
        message: format!("Parámetros inválidos en tools/call: {e}"),
        data: None,
    })?;

    let result = match call.name.as_str() {
        "get_document_index" => {
            let args: GetDocumentIndexArgs =
                serde_json::from_value(call.arguments).map_err(|e| JsonRpcError {
                    code: -32602,
                    message: format!("Argumentos inválidos para get_document_index: {e}"),
                    data: None,
                })?;
            handle_get_document_index(args).and_then(|res| wrap_tool_ok(&res))
        }
        "prepare_module_edit" => {
            let args: PrepareModuleEditArgs =
                serde_json::from_value(call.arguments).map_err(|e| JsonRpcError {
                    code: -32602,
                    message: format!("Argumentos inválidos para prepare_module_edit: {e}"),
                    data: None,
                })?;
            handle_prepare_module_edit(args).and_then(|res| wrap_tool_ok(&res))
        }
        "apply_module_edit" => {
            let args: ApplyModuleEditArgs =
                serde_json::from_value(call.arguments).map_err(|e| JsonRpcError {
                    code: -32602,
                    message: format!("Argumentos inválidos para apply_module_edit: {e}"),
                    data: None,
                })?;
            handle_apply_module_edit(args).and_then(|res| wrap_tool_ok(&res))
        }
        unknown => Err(anyhow!("Tool desconocida: '{unknown}'")),
    };

    match result {
        Ok(val) => Ok(val),
        Err(err) => Ok(wrap_tool_err(err)),
    }
}

/// Procesa una petición JSON-RPC 2.0 y devuelve `Some(JsonRpcResponse)` si requiere respuesta.
pub fn handle_jsonrpc_request(req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    // Las notificaciones JSON-RPC (sin `id`) no generan respuesta.
    let id = req.id?;

    let outcome = match req.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "md-surgeon-mcp",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "For any Markdown (.md) planning, design, architecture, or documentation file, ALWAYS prioritize the 3-step md-surgeon workflow (get_document_index -> prepare_module_edit -> apply_module_edit) instead of reading or rewriting entire files with read_file/edit_file. This guarantees surgical byte-accurate edits, prevents token overflow, and preserves document structure."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(get_tools_list()),
        "tools/call" => dispatch_tool_call(req.params),
        other => Err(JsonRpcError {
            code: -32601,
            message: format!("Método no encontrado: '{other}'"),
            data: None,
        }),
    };

    match outcome {
        Ok(result) => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }),
        Err(error) => Some(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }),
    }
}

/// Inicia el bucle principal del servidor MCP escuchando peticiones JSON-RPC 2.0 por `stdio`.
pub fn run_stdio_server() -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line.context("Error leyendo línea desde stdin")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => handle_jsonrpc_request(req),
            Err(err) => Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("Error de parseo JSON: {err}"),
                    data: None,
                }),
            }),
        };

        if let Some(resp) = response {
            let serialized = serde_json::to_string(&resp)?;
            writeln!(stdout, "{serialized}")?;
            stdout.flush()?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_temp_markdown_file(contents: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!("md_surgeon_test_{nanos}.md"));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn test_end_to_end_mcp_tools_flow() {
        let initial_md = "\
## 1. Visión
Sistema de mensajería distribuida de baja latencia.

## 2. Almacenamiento
Motor basado en segmentos de log en disco.

### Índices
Índice disperso en memoria para búsqueda rápida por offset.

### Retención
Política de compactación por clave y TTL de 7 días.
";
        let temp_path = create_temp_markdown_file(initial_md);
        let file_path_str = temp_path.to_string_lossy().to_string();

        // 0. Verificar initialize con instructions y annotations en `tools/list`
        let init_req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(0)),
            method: "initialize".to_string(),
            params: Some(json!({})),
        };
        let init_resp = handle_jsonrpc_request(init_req).unwrap();
        let init_val = init_resp.result.unwrap();
        assert!(init_val["instructions"].as_str().unwrap().contains("md-surgeon"));

        let list_req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        let list_resp = handle_jsonrpc_request(list_req).unwrap();
        let tools_arr = list_resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(tools_arr[0]["description"].as_str().unwrap().starts_with("ALWAYS CALL THIS FIRST"));
        assert_eq!(tools_arr[0]["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(tools_arr[1]["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(tools_arr[2]["annotations"]["readOnlyHint"], json!(false));
        assert_eq!(tools_arr[2]["annotations"]["idempotentHint"], json!(true));

        // 1. Probar `get_document_index`
        let index_res = handle_get_document_index(GetDocumentIndexArgs {
            file_path: file_path_str.clone(),
        })
        .unwrap();
        assert_eq!(index_res.modules.len(), 2);
        assert_eq!(index_res.modules[1].id, "2-almacenamiento");
        assert_eq!(index_res.modules[1].submodules.len(), 2);
        assert_eq!(
            index_res.modules[1].submodules[0].id,
            "2-almacenamiento/indices"
        );

        // 2. Probar `prepare_module_edit` por lotes (`target_ids`) vía JSON-RPC `tools/call`
        let prepare_req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(10)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "prepare_module_edit",
                "arguments": {
                    "file_path": file_path_str,
                    "target_ids": ["2-almacenamiento/indices", "2-almacenamiento/retencion"],
                    "guide_ids": ["1-vision"]
                }
            })),
        };
        let prepare_resp = handle_jsonrpc_request(prepare_req).unwrap();
        assert!(prepare_resp.error.is_none());
        let tool_text = prepare_resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let bundle: ContextBundle = serde_json::from_str(&tool_text).unwrap();
        assert_eq!(bundle.targets.len(), 2);
        assert!(
            bundle.targets["2-almacenamiento/indices"].contains("Índice disperso en memoria")
        );
        assert!(
            bundle.targets["2-almacenamiento/retencion"].contains("TTL de 7 días")
        );
        assert!(bundle.guides["1-vision"].contains("baja latencia"));

        // 3. Probar `apply_module_edit` en lote (`edits`) vía JSON-RPC `tools/call`
        let apply_req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(11)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "apply_module_edit",
                "arguments": {
                    "file_path": file_path_str,
                    "edits": [
                        {
                            "target_id": "2-almacenamiento/indices",
                            "new_markdown_content": "### Índices\nÍndice B+Tree mapeado con `mmap` para lecturas O(1)."
                        },
                        {
                            "target_id": "2-almacenamiento/retencion",
                            "new_markdown_content": "### Retención\nPolítica de tiering automático a S3 a los 30 días."
                        }
                    ]
                }
            })),
        };
        let apply_resp = handle_jsonrpc_request(apply_req).unwrap();
        assert!(apply_resp.error.is_none());

        // 4. Verificar que el archivo en disco fue parcheado quirúrgicamente para ambos submódulos
        let final_disk_md = fs::read_to_string(&temp_path).unwrap();
        assert!(final_disk_md.contains("Índice B+Tree mapeado con `mmap`"));
        assert!(!final_disk_md.contains("Índice disperso en memoria"));
        assert!(final_disk_md.contains("Política de tiering automático a S3 a los 30 días."));
        assert!(!final_disk_md.contains("TTL de 7 días"));
        assert!(final_disk_md.contains("## 1. Visión"));

        let _ = fs::remove_file(temp_path);
    }
}
