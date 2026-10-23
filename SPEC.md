# `md-surgeon-mcp` — Especificación Técnica y Contrato de Arquitectura (`SPEC.md`)

## 1. Propósito del Sistema

`md-surgeon-mcp` es un servidor MCP (*Model Context Protocol*) en Rust diseñado para consultar y editar quirúrgicamente documentos Markdown extensos de planificación sin cargar ni reescribir el documento completo en la ventana de contexto del agente.

El sistema se divide en dos capas desacopladas:
- **`src/parser.rs`**: Motor puro de análisis sintáctico y manipulación de buffers en memoria basado en offsets exactos de bytes (`pulldown-cmark`).
- **`src/mcp.rs`**: Capa de transporte JSON-RPC 2.0 sobre `stdio`, metadatos de herramientas (`annotations`) y adaptación de operaciones por lotes (*batch*).

---

## 2. Definición de Módulos, Submódulos y Rangos de Bytes (`byte_range`)

### 2.1 Eventos y Niveles Soportados
El parser utiliza `pulldown_cmark::Parser::new(markdown).into_offset_iter()` para obtener pares `(Event, Range<usize>)` con posiciones exactas en bytes dentro del slice UTF-8 original.

- Se indexan exclusivamente los encabezados de nivel:
  - `H1` (`#`) -> `level = 1`
  - `H2` (`##`) -> `level = 2`
  - `H3` (`###`) -> `level = 3`
- Encabezados de nivel inferior (`####`, `#####`, `######`) se tratan como contenido plano perteneciente al módulo o submódulo que los contiene y no generan nodos en el árbol ni cierran el bloque actual.

### 2.2 Límites Exactos (`start..end`)
Para cada encabezado indexado `H` de nivel $L \in \{1, 2, 3\}$:
- **Byte de inicio (`start`)**: Es el offset exacto (`range.start`) donde comienza el evento `Event::Start(Tag::Heading { level, .. })` en el documento original.
- **Byte de fin (`end`)**: Es el offset `next_range.start` del primer encabezado posterior cuyo nivel $L_{\text{next}}$ cumpla $L_{\text{next}} \le L$ (es decir, un encabezado de nivel jerárquico igual o superior). Si no existe tal encabezado hasta el final del documento, `end = markdown.len()`.

### 2.3 Propiedad de Contención e Independencia Quirúrgica
- Un módulo `##` (`level = 2`) tiene un `byte_range` que abarca su propio texto introductorio **más** la totalidad de sus submódulos `###` (`level = 3`) hijos hasta el próximo `##` o `#`.
- Cada submódulo `###` (`level = 3`) posee su propio `byte_range` acotado desde su `###` hasta el siguiente `###`, `##` o `#`.
- Gracias a esto:
  - Si un `target_id` apunta a un `##`, se lee o reemplaza el módulo completo incluyendo sus submódulos.
  - Si un `target_id` apunta a un `###` (`padre/hijo`), se lee o reemplaza **únicamente** la subsección `###` sin alterar el encabezado ni el resto del `##` padre.

---

## 3. Identificadores Jerárquicos (`slug`)

### 3.1 Algoritmo de Normalización (`slugify`)
Dado el texto plano del título de un encabezado:
1. Convertir a minúsculas (`to_lowercase()`).
2. Normalizar caracteres latinos acentuados y eñes (`á->a`, `é->e`, `í->i`, `ó->o`, `ú->u`, `ü->u`, `ñ->n`).
3. Reemplazar cualquier carácter no alfanumérico ASCII (`!c.is_ascii_alphanumeric()`) por un guion `-`, colapsando múltiples guiones consecutivos en uno solo.
4. Eliminar guiones al inicio y al final (`trim_matches('-')`).

### 3.2 Reglas de Direccionamiento Jerárquico
- **Módulos Principales (`#` y `##`)**:
  Usan directamente su `slug` como `id`.
  - Ejemplo: `## 2. Base de Datos` -> `id = "2-base-de-datos"`.
- **Submódulos (`###`)**:
  Usan una ruta compuesta `<slug-padre>/<slug-hijo>` utilizando el `id` de su módulo `##` (o `#` si cuelga directamente de un `#`) contenedor.
  - Ejemplo: `### Tabla Pagos` dentro de `## 2. Base de Datos` -> `id = "2-base-de-datos/tabla-pagos"`.

---

## 4. Extracción de Descripción Corta (`summary`)

Para cada módulo o submódulo indexado:
1. Se captura el texto plano del **primer párrafo (`Tag::Paragraph`) o cita (`Tag::BlockQuote`)** que aparezca inmediatamente debajo del encabezado y **antes** de que inicie cualquier sub-encabezado hijo (`#`, `##`, `###`).
2. Se eliminan todos los saltos de línea (`\r`, `\n`) y se colapsan secuencias de espacios en blanco en un único espacio `' '`.
3. Se extrae la primera oración (cortando en el primer punto seguido de espacio `". "` o punto final de bloque) o el texto completo del bloque si no contiene punto.
4. Se aplica un límite máximo estricto de **120 caracteres** (`chars().count() <= 120`), respetando fronteras de caracteres UTF-8.
5. Si el encabezado no tiene párrafo ni `blockquote` antes del siguiente encabezado o fin de sección, `summary` es `""` (cadena vacía).
6. **Restricción expresa**: No se calcula ni devuelve conteo de tokens en ninguna estructura.

---

## 5. Contrato Público de `src/parser.rs`

### 5.1 Estructuras de Datos

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleNode {
    pub id: String,
    pub title: String,
    pub level: u8,
    pub summary: String,
    #[serde(skip)]
    pub byte_range: Range<usize>,
    pub submodules: Vec<ModuleNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBundle {
    pub targets: BTreeMap<String, String>,
    pub guides: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleEdit {
    pub target_id: String,
    #[serde(alias = "new_markdown_content")]
    pub new_content: String,
}
```

### 5.2 Funciones Públicas

- `pub fn get_document_tree(markdown: &str) -> Vec<ModuleNode>`
  Recorre `markdown` en una sola pasada con `into_offset_iter`, construyendo el árbol jerárquico de nodos con su `id`, `title`, `level`, `summary`, `byte_range` interno y `submodules`.

- `pub fn extract_context_bundle(markdown: &str, target_ids: &[String], guide_ids: &[String]) -> Result<ContextBundle>`
  Busca en el árbol del documento todos los IDs solicitados en `target_ids` y `guide_ids`. Devuelve `targets: BTreeMap<String, String>` y `guides: BTreeMap<String, String>` con sus tajadas exactas `&markdown[node.byte_range]`.

- `pub fn replace_multiple_modules(markdown: &str, edits: &[ModuleEdit]) -> Result<String>`
  Resuelve los `byte_range` de todos los `target_id` sobre el árbol actual, verifica que ningún par de rangos se solape entre sí (devolviendo `Err` si se intenta editar simultáneamente un padre y su hijo), **ordena las ediciones de forma descendente por `byte_range.start` (de abajo hacia arriba)** y aplica los reemplazos en ese orden inverso sin invalidar los offsets superiores. Si `new_content` está vacío (`""`), elimina el bloque limpiamente sin dejar saltos de línea triples huérfanos.

- `pub fn replace_module_content(markdown: &str, target_id: &str, new_content: &str) -> Result<String>`
  Envoltorio de conveniencia sobre `replace_multiple_modules` para una única edición.

---

## 6. Contrato JSON-RPC 2.0 y Herramientas MCP (`src/mcp.rs`)

El servidor se comunica por `stdio` (`stdin`/`stdout`) usando mensajes JSON-RPC 2.0 delimitados por salto de línea (`\n`). En la respuesta al método `initialize`, el servidor inyecta el campo estándar `"instructions"` con directivas claras para que cualquier agente de IA priorice el flujo quirúrgico de 3 pasos (`get_document_index` -> `prepare_module_edit` -> `apply_module_edit`) sobre `read_file`/`edit_file`. Cada herramienta incluye además el bloque oficial `"annotations"` de MCP en `tools/list`.

### 6.1 `get_document_index`
- **Annotations**: `{ "readOnlyHint": true, "destructiveHint": false }`
- **Argumentos**: `{ "file_path": "PLAN.md" }`

### 6.2 `prepare_module_edit` (Soporte Batch + Retrocompatibilidad)
Extrae en una sola llamada el contenido actual de múltiples módulos/submódulos a editar (`target_ids`) y el mapa de contenidos de referencia (`guide_ids`). Acepta opcionalmente `target_id` individual por retrocompatibilidad.

- **Annotations**: `{ "readOnlyHint": true, "destructiveHint": false }`
- **Respuesta (`ContextBundle`)**:
  ```json
  {
    "targets": {
      "2-base-de-datos/tabla-pagos": "### Tabla Pagos\nAlmacena las transacciones confirmadas...\n",
      "2-base-de-datos/tabla-usuarios": "### Tabla Usuarios\nContiene las cuentas verificadas...\n"
    },
    "guides": {
      "1-arquitectura": "## 1. Arquitectura\nEl sistema sigue una arquitectura hexagonal...\n"
    }
  }
  ```

### 6.3 `apply_module_edit` (Soporte Batch + Eliminación Limpia)
Parchea en disco `N` módulos o submódulos en una sola operación atómica de disco recibiendo `edits: Vec<ModuleEdit>` (y manteniendo retrocompatibilidad con `target_id` + `new_markdown_content` en la raíz).

- **Annotations**: `{ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true }`
- **Respuesta (`ApplyModuleEditResponse`)**:
  ```json
  {
    "file_path": "PLAN.md",
    "updated_targets": [
      "2-base-de-datos/tabla-pagos",
      "2-base-de-datos/tabla-usuarios"
    ],
    "updated": true,
    "bytes_written": 4820
  }
  ```
