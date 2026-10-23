use std::collections::BTreeMap;
use std::ops::Range;

use anyhow::{Result, anyhow};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

/// Límite máximo de caracteres para el resumen (`summary`) de cada módulo o submódulo.
const MAX_SUMMARY_CHARS: usize = 120;

/// Representa un módulo (`#`, `##`) o submódulo (`###`) dentro del árbol del documento.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleNode {
    /// Identificador único jerárquico (ej. `2-base-de-datos` o `2-base-de-datos/tabla-pagos`).
    pub id: String,
    /// Texto limpio del encabezado sin los símbolos `#`.
    pub title: String,
    /// Nivel del encabezado (`1`, `2` o `3`).
    pub level: u8,
    /// Primera oración o primer párrafo/blockquote inmediato (máx. 120 caracteres, sin saltos de línea).
    pub summary: String,
    /// Rango exacto de bytes `start..end` en el documento original (uso interno del parser).
    #[serde(skip)]
    pub byte_range: Range<usize>,
    /// Submódulos hijos anidados bajo este módulo.
    pub submodules: Vec<ModuleNode>,
}

/// Paquete de contexto devuelto al preparar la edición quirúrgica de uno o varios módulos/submódulos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBundle {
    /// Mapa ordenado de `target_id -> contenido Markdown actual` de los módulos a editar.
    pub targets: BTreeMap<String, String>,
    /// Mapa ordenado de `guide_id -> contenido Markdown` para contexto de solo lectura.
    pub guides: BTreeMap<String, String>,
}

/// Operación individual de edición dentro de un lote (`batch`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleEdit {
    /// ID jerárquico del módulo o submódulo a modificar (o eliminar si `new_content` está vacío).
    pub target_id: String,
    /// Nuevo contenido Markdown completo de la sección (o `""` para eliminar el bloque).
    #[serde(alias = "new_markdown_content")]
    pub new_content: String,
}

/// Representación plana intermedia de una sección antes de construir la jerarquía.
#[derive(Debug, Clone)]
struct RawSection {
    level: u8,
    title: String,
    start_byte: usize,
    end_byte: usize,
    summary: String,
}

/// Convierte un nivel de encabezado de `pulldown-cmark` a `u8` si está en el rango `H1..=H3`.
fn map_heading_level(level: HeadingLevel) -> Option<u8> {
    match level {
        HeadingLevel::H1 => Some(1),
        HeadingLevel::H2 => Some(2),
        HeadingLevel::H3 => Some(3),
        HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6 => None,
    }
}

/// Genera un `slug` limpio en minúsculas y ASCII a partir de un título de encabezado.
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = false;

    for ch in title.chars() {
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        let mapped = match lower {
            'á' | 'à' | 'ä' | 'â' | 'ã' => Some('a'),
            'é' | 'è' | 'ë' | 'ê' => Some('e'),
            'í' | 'ì' | 'ï' | 'î' => Some('i'),
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => Some('o'),
            'ú' | 'ù' | 'ü' | 'û' => Some('u'),
            'ñ' => Some('n'),
            'ç' => Some('c'),
            c if c.is_ascii_alphanumeric() => Some(c),
            _ => None,
        };

        if let Some(valid_char) = mapped {
            slug.push(valid_char);
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

/// Limpia saltos de línea, extrae la primera oración y trunca a un máximo de 120 caracteres.
fn format_summary(raw: &str) -> String {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }

    // Extraer la primera oración (hasta el primer ". " o el final del bloque).
    let sentence = if let Some(dot_pos) = collapsed.find(". ") {
        &collapsed[..=dot_pos]
    } else {
        collapsed.as_str()
    };

    let char_count = sentence.chars().count();
    if char_count <= MAX_SUMMARY_CHARS {
        sentence.to_string()
    } else {
        let truncated: String = sentence.chars().take(MAX_SUMMARY_CHARS - 3).collect();
        format!("{}...", truncated.trim_end())
    }
}

/// Extrae la lista plana de secciones (`#`, `##`, `###`) con sus offsets exactos y resúmenes.
fn parse_raw_sections(markdown: &str) -> Vec<RawSection> {
    let mut sections: Vec<RawSection> = Vec::new();

    let mut current_heading_level: Option<u8> = None;
    let mut current_heading_start: usize = 0;
    let mut current_heading_title = String::new();

    let mut awaiting_summary = false;
    let mut capturing_summary_paragraph = false;
    let mut current_summary_buf = String::new();

    for (event, range) in Parser::new(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                // Cualquier encabezado detiene la búsqueda de resumen de la sección previa.
                awaiting_summary = false;
                capturing_summary_paragraph = false;
                current_summary_buf.clear();

                if let Some(lvl) = map_heading_level(level) {
                    current_heading_level = Some(lvl);
                    current_heading_start = range.start;
                    current_heading_title.clear();
                } else {
                    current_heading_level = None;
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(lvl) = current_heading_level.take() {
                    sections.push(RawSection {
                        level: lvl,
                        title: current_heading_title.trim().to_string(),
                        start_byte: current_heading_start,
                        end_byte: markdown.len(),
                        summary: String::new(),
                    });
                    awaiting_summary = true;
                }
            }
            Event::Start(Tag::Paragraph) => {
                if awaiting_summary && !capturing_summary_paragraph {
                    capturing_summary_paragraph = true;
                    current_summary_buf.clear();
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if capturing_summary_paragraph {
                    if let Some(last_section) = sections.last_mut() {
                        last_section.summary = format_summary(&current_summary_buf);
                    }
                    capturing_summary_paragraph = false;
                    awaiting_summary = false;
                    current_summary_buf.clear();
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                // Un blockquote inmediato debajo del encabezado contiene párrafos que serán
                // capturados por `Event::Start(Tag::Paragraph)` mientras `awaiting_summary` sea true.
            }
            Event::Start(
                Tag::CodeBlock(_) | Tag::List(_) | Tag::Table(_) | Tag::HtmlBlock,
            ) => {
                // Si antes de cualquier párrafo o blockquote aparece otro bloque estructural,
                // ya no existe un párrafo/blockquote inmediato debajo del encabezado.
                if awaiting_summary && !capturing_summary_paragraph {
                    awaiting_summary = false;
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if current_heading_level.is_some() {
                    current_heading_title.push_str(&text);
                } else if capturing_summary_paragraph {
                    current_summary_buf.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if current_heading_level.is_some() {
                    current_heading_title.push(' ');
                } else if capturing_summary_paragraph {
                    current_summary_buf.push(' ');
                }
            }
            _ => {}
        }
    }

    // Calcular `end_byte` para cada sección: termina exactamente en el `start_byte`
    // del siguiente encabezado de nivel igual o superior (`next.level <= curr.level`).
    let len = sections.len();
    for i in 0..len {
        let curr_level = sections[i].level;
        let mut computed_end = markdown.len();
        for next in sections.iter().skip(i + 1) {
            if next.level <= curr_level {
                computed_end = next.start_byte;
                break;
            }
        }
        sections[i].end_byte = computed_end;
    }

    sections
}

/// Construye el árbol jerárquico de módulos y submódulos del documento Markdown.
pub fn get_document_tree(markdown: &str) -> Vec<ModuleNode> {
    let raw_sections = parse_raw_sections(markdown);
    let mut roots: Vec<ModuleNode> = Vec::new();

    for raw in raw_sections {
        let base_slug = slugify(&raw.title);
        let byte_range = raw.start_byte..raw.end_byte;

        match raw.level {
            1 => {
                roots.push(ModuleNode {
                    id: base_slug,
                    title: raw.title,
                    level: 1,
                    summary: raw.summary,
                    byte_range,
                    submodules: Vec::new(),
                });
            }
            2 => {
                // Los módulos principales `##` usan su propio slug como `id`.
                let node = ModuleNode {
                    id: base_slug,
                    title: raw.title,
                    level: 2,
                    summary: raw.summary,
                    byte_range,
                    submodules: Vec::new(),
                };

                if let Some(last_h1) = roots.last_mut().filter(|n| n.level == 1) {
                    last_h1.submodules.push(node);
                } else {
                    roots.push(node);
                }
            }
            3 => {
                // Determinar el nodo padre (`##` o `#`) para formar el ID compuesto `padre/hijo`.
                if let Some(last_root) = roots.last_mut() {
                    if last_root.level == 2 {
                        let composite_id = format!("{}/{}", last_root.id, base_slug);
                        last_root.submodules.push(ModuleNode {
                            id: composite_id,
                            title: raw.title,
                            level: 3,
                            summary: raw.summary,
                            byte_range,
                            submodules: Vec::new(),
                        });
                        continue;
                    } else if last_root.level == 1 {
                        if let Some(last_h2) =
                            last_root.submodules.last_mut().filter(|n| n.level == 2)
                        {
                            let composite_id = format!("{}/{}", last_h2.id, base_slug);
                            last_h2.submodules.push(ModuleNode {
                                id: composite_id,
                                title: raw.title,
                                level: 3,
                                summary: raw.summary,
                                byte_range,
                                submodules: Vec::new(),
                            });
                            continue;
                        } else {
                            let composite_id = format!("{}/{}", last_root.id, base_slug);
                            last_root.submodules.push(ModuleNode {
                                id: composite_id,
                                title: raw.title,
                                level: 3,
                                summary: raw.summary,
                                byte_range,
                                submodules: Vec::new(),
                            });
                            continue;
                        }
                    }
                }

                // Caso huérfano: un `###` sin `#` ni `##` previo.
                roots.push(ModuleNode {
                    id: base_slug,
                    title: raw.title,
                    level: 3,
                    summary: raw.summary,
                    byte_range,
                    submodules: Vec::new(),
                });
            }
            _ => {}
        }
    }

    roots
}

/// Busca recursivamente un nodo por su `id` dentro del árbol de módulos.
fn find_node_by_id<'a>(nodes: &'a [ModuleNode], target_id: &str) -> Option<&'a ModuleNode> {
    for node in nodes {
        if node.id == target_id {
            return Some(node);
        }
        if let Some(found) = find_node_by_id(&node.submodules, target_id) {
            return Some(found);
        }
    }
    None
}

/// Extrae el contenido exacto de múltiples módulos/submódulos objetivo (`target_ids`)
/// y de los módulos guía de solo lectura (`guide_ids`).
pub fn extract_context_bundle(
    markdown: &str,
    target_ids: &[String],
    guide_ids: &[String],
) -> Result<ContextBundle> {
    if target_ids.is_empty() {
        return Err(anyhow!(
            "Debe especificar al menos un ID en 'target_ids' para preparar la edición"
        ));
    }

    let tree = get_document_tree(markdown);

    let mut targets = BTreeMap::new();
    for target_id in target_ids {
        let target_node = find_node_by_id(&tree, target_id).ok_or_else(|| {
            anyhow!("No se encontró el módulo o submódulo objetivo '{target_id}'")
        })?;
        let target_content = markdown[target_node.byte_range.clone()].to_string();
        targets.insert(target_id.clone(), target_content);
    }

    let mut guides = BTreeMap::new();
    for guide_id in guide_ids {
        let guide_node = find_node_by_id(&tree, guide_id).ok_or_else(|| {
            anyhow!("No se encontró el módulo o submódulo guía '{guide_id}'")
        })?;
        let guide_content = markdown[guide_node.byte_range.clone()].to_string();
        guides.insert(guide_id.clone(), guide_content);
    }

    Ok(ContextBundle { targets, guides })
}

/// Aplica múltiples ediciones quirúrgicas en una sola pasada ordenando los rangos de abajo hacia arriba
/// (`byte_range.start` descendente) tras verificar que ningún rango se solape entre sí.
pub fn replace_multiple_modules(markdown: &str, edits: &[ModuleEdit]) -> Result<String> {
    if edits.is_empty() {
        return Err(anyhow!(
            "Debe proporcionar al menos una operación de edición en 'edits'"
        ));
    }

    let tree = get_document_tree(markdown);

    #[derive(Debug, Clone)]
    struct ResolvedEdit<'a> {
        target_id: &'a str,
        new_content: &'a str,
        range: Range<usize>,
    }

    let mut resolved: Vec<ResolvedEdit<'_>> = Vec::with_capacity(edits.len());
    for edit in edits {
        let node = find_node_by_id(&tree, &edit.target_id).ok_or_else(|| {
            anyhow!(
                "No se encontró el módulo o submódulo objetivo '{}'",
                edit.target_id
            )
        })?;
        resolved.push(ResolvedEdit {
            target_id: &edit.target_id,
            new_content: &edit.new_content,
            range: node.byte_range.clone(),
        });
    }

    // Verificar que ningún par de rangos se solape (ej. editar padre e hijo simultáneamente o IDs duplicados).
    for i in 0..resolved.len() {
        for j in (i + 1)..resolved.len() {
            let a = &resolved[i];
            let b = &resolved[j];
            if a.range.start < b.range.end && b.range.start < a.range.end {
                return Err(anyhow!(
                    "Conflicto de rangos solapados entre '{}' ({}..{}) y '{}' ({}..{})",
                    a.target_id,
                    a.range.start,
                    a.range.end,
                    b.target_id,
                    b.range.start,
                    b.range.end
                ));
            }
        }
    }

    // Ordenar de forma descendente por `byte_range.start` (de abajo hacia arriba en el documento)
    // para que las mutaciones inferiores nunca desplacen los offsets de los módulos superiores.
    resolved.sort_by(|a, b| b.range.start.cmp(&a.range.start));

    let mut result = markdown.to_string();

    for edit in resolved {
        let range = edit.range;
        if edit.new_content.is_empty() {
            // Eliminación limpia del módulo/submódulo sin dejar saltos de línea triples huérfanos.
            if range.end == result.len() {
                result.truncate(range.start);
                let trimmed_len = result.trim_end_matches(['\r', '\n']).len();
                if trimmed_len == 0 {
                    result.clear();
                } else {
                    result.truncate(trimmed_len);
                    result.push('\n');
                }
            } else {
                let after_trimmed_start = result[range.end..]
                    .find(|c: char| c != '\r' && c != '\n')
                    .map(|offset| range.end + offset)
                    .unwrap_or(result.len());
                result.replace_range(range.start..after_trimmed_start, "");
            }
        } else {
            let original_slice = &result[range.clone()];
            let trailing_newlines = if !edit.new_content.ends_with('\n') {
                let trimmed_len = original_slice.trim_end_matches(['\r', '\n']).len();
                let suffix = &original_slice[trimmed_len..];
                if suffix.is_empty() && range.end < result.len() {
                    "\n\n"
                } else {
                    suffix
                }
            } else {
                ""
            };

            let mut replacement =
                String::with_capacity(edit.new_content.len() + trailing_newlines.len());
            replacement.push_str(edit.new_content);
            replacement.push_str(trailing_newlines);

            result.replace_range(range, &replacement);
        }
    }

    Ok(result)
}

/// Reemplaza quirúrgicamente el rango de bytes `start..end` del módulo o submódulo `target_id` con `new_content`.
pub fn replace_module_content(
    markdown: &str,
    target_id: &str,
    new_content: &str,
) -> Result<String> {
    let edits = [ModuleEdit {
        target_id: target_id.to_string(),
        new_content: new_content.to_string(),
    }];
    replace_multiple_modules(markdown, &edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DOC: &str = "\
# Plan de Plataforma Financiera
 > Documento maestro de arquitectura para el motor de pagos en tiempo real.

## 1. Arquitectura General
El sistema emplea una arquitectura hexagonal orientada a eventos. Todos los servicios se comunican por colas.

## 2. Base de Datos
Especificación del modelo relacional y particionamiento en PostgreSQL.

### Tabla Pagos
Almacena las transacciones confirmadas y su estado de conciliación.
- `id`: UUID
- `monto`: NUMERIC(12, 2)

### Tabla Usuarios
Contiene las cuentas verificadas y perfiles KYC.
- `id`: UUID
- `email`: TEXT

## 3. Seguridad y Auditoría
### Tabla Pagos
Registro inmutable en bóveda de auditoría para cada pago.
";

    #[test]
    fn test_slugify_accents_and_punctuation() {
        assert_eq!(slugify("2. Base de Datos"), "2-base-de-datos");
        assert_eq!(slugify("Tabla Pagos"), "tabla-pagos");
        assert_eq!(
            slugify("3. Seguridad y Auditoría (PCI-DSS)"),
            "3-seguridad-y-auditoria-pci-dss"
        );
    }

    #[test]
    fn test_get_document_tree_hierarchy_and_summaries() {
        let tree = get_document_tree(SAMPLE_DOC);
        assert_eq!(tree.len(), 1);

        let root = &tree[0];
        assert_eq!(root.id, "plan-de-plataforma-financiera");
        assert_eq!(root.level, 1);
        assert_eq!(
            root.summary,
            "Documento maestro de arquitectura para el motor de pagos en tiempo real."
        );
        assert_eq!(root.submodules.len(), 3);

        let mod_arch = &root.submodules[0];
        assert_eq!(mod_arch.id, "1-arquitectura-general");
        assert_eq!(
            mod_arch.summary,
            "El sistema emplea una arquitectura hexagonal orientada a eventos."
        );
        assert!(mod_arch.submodules.is_empty());

        let mod_db = &root.submodules[1];
        assert_eq!(mod_db.id, "2-base-de-datos");
        assert_eq!(
            mod_db.summary,
            "Especificación del modelo relacional y particionamiento en PostgreSQL."
        );
        assert_eq!(mod_db.submodules.len(), 2);

        let sub_pagos = &mod_db.submodules[0];
        assert_eq!(sub_pagos.id, "2-base-de-datos/tabla-pagos");
        assert_eq!(
            sub_pagos.summary,
            "Almacena las transacciones confirmadas y su estado de conciliación."
        );

        let sub_usuarios = &mod_db.submodules[1];
        assert_eq!(sub_usuarios.id, "2-base-de-datos/tabla-usuarios");
        assert_eq!(
            sub_usuarios.summary,
            "Contiene las cuentas verificadas y perfiles KYC."
        );

        // El módulo 3 no tiene párrafo antes de su primer `###`, por lo que su summary es vacío,
        // y su submódulo `### Tabla Pagos` no colisiona gracias al prefijo del padre.
        let mod_sec = &root.submodules[2];
        assert_eq!(mod_sec.id, "3-seguridad-y-auditoria");
        assert_eq!(mod_sec.summary, "");
        assert_eq!(mod_sec.submodules.len(), 1);
        assert_eq!(
            mod_sec.submodules[0].id,
            "3-seguridad-y-auditoria/tabla-pagos"
        );
    }

    #[test]
    fn test_summary_truncation_at_120_chars() {
        let long_doc = format!(
            "## Sección Larga\n{}\n",
            " palabra".repeat(30) // > 120 chars sin punto intermedio
        );
        let tree = get_document_tree(&long_doc);
        assert_eq!(tree.len(), 1);
        assert!(tree[0].summary.chars().count() <= 120);
        assert!(tree[0].summary.ends_with("..."));
    }

    #[test]
    fn test_extract_context_bundle_module_and_submodule() {
        let targets = vec![
            "2-base-de-datos/tabla-pagos".to_string(),
            "2-base-de-datos/tabla-usuarios".to_string(),
        ];
        let guides = vec![
            "1-arquitectura-general".to_string(),
            "3-seguridad-y-auditoria/tabla-pagos".to_string(),
        ];
        let bundle = extract_context_bundle(SAMPLE_DOC, &targets, &guides).unwrap();

        assert_eq!(bundle.targets.len(), 2);
        assert!(
            bundle.targets["2-base-de-datos/tabla-pagos"].starts_with("### Tabla Pagos\n")
        );
        assert!(
            bundle.targets["2-base-de-datos/tabla-pagos"].contains("`monto`: NUMERIC(12, 2)")
        );
        assert!(
            !bundle.targets["2-base-de-datos/tabla-pagos"].contains("### Tabla Usuarios")
        );
        assert!(
            bundle.targets["2-base-de-datos/tabla-usuarios"]
                .contains("Contiene las cuentas verificadas")
        );

        assert_eq!(bundle.guides.len(), 2);
        assert!(
            bundle.guides["1-arquitectura-general"]
                .starts_with("## 1. Arquitectura General\n")
        );
        assert!(
            bundle.guides["3-seguridad-y-auditoria/tabla-pagos"]
                .contains("Registro inmutable en bóveda")
        );
    }

    #[test]
    fn test_replace_submodule_preserves_parent_and_siblings() {
        let new_pagos = "### Tabla Pagos\nAlmacena transacciones multi-divisa.\n- `id`: UUID\n- `moneda`: CHAR(3)";
        let updated =
            replace_module_content(SAMPLE_DOC, "2-base-de-datos/tabla-pagos", new_pagos).unwrap();

        // Verifica que el submódulo objetivo cambió
        assert!(updated.contains("Almacena transacciones multi-divisa."));
        assert!(updated.contains("- `moneda`: CHAR(3)"));
        assert!(!updated.contains("- `monto`: NUMERIC(12, 2)"));

        // Verifica que el padre `## 2. Base de Datos`, el hermano `### Tabla Usuarios`
        // y el homónimo `3-seguridad-y-auditoria/tabla-pagos` permanecen intactos.
        assert!(updated.contains("## 2. Base de Datos\nEspecificación del modelo relacional"));
        assert!(updated.contains("\n\n### Tabla Usuarios\nContiene las cuentas verificadas"));
        assert!(updated.contains("Registro inmutable en bóveda de auditoría para cada pago."));
    }

    #[test]
    fn test_replace_entire_parent_module() {
        let new_db_module = "## 2. Base de Datos\nMigrado completamente a CockroachDB.\n\n### Tabla Ledger\n- `tx_id`: UUID";
        let updated =
            replace_module_content(SAMPLE_DOC, "2-base-de-datos", new_db_module).unwrap();

        assert!(updated.contains("Migrado completamente a CockroachDB."));
        assert!(updated.contains("### Tabla Ledger"));
        assert!(!updated.contains("### Tabla Usuarios"));
        assert!(updated.contains("## 1. Arquitectura General"));
        assert!(updated.contains("\n\n## 3. Seguridad y Auditoría"));
    }

    #[test]
    fn test_batch_replace_multiple_modules_bottom_up() {
        let edits = vec![
            // 1. Modificar el primer módulo `## 1. Arquitectura General`
            ModuleEdit {
                target_id: "1-arquitectura-general".to_string(),
                new_content: "## 1. Arquitectura General\nArquitectura orientada a microservicios gRPC.".to_string(),
            },
            // 2. Modificar un submódulo intermedio `2-base-de-datos/tabla-pagos`
            ModuleEdit {
                target_id: "2-base-de-datos/tabla-pagos".to_string(),
                new_content: "### Tabla Pagos\nAlmacena liquidaciones instantáneas SPEI/PIX.".to_string(),
            },
            // 3. Eliminar por completo el último módulo `## 3. Seguridad y Auditoría`
            ModuleEdit {
                target_id: "3-seguridad-y-auditoria".to_string(),
                new_content: String::new(),
            },
        ];

        let updated = replace_multiple_modules(SAMPLE_DOC, &edits).unwrap();

        // El primer módulo fue actualizado
        assert!(updated.contains("## 1. Arquitectura General\nArquitectura orientada a microservicios gRPC.\n\n## 2. Base de Datos"));
        // El submódulo intermedio fue actualizado sin afectar a su hermano `### Tabla Usuarios`
        assert!(updated.contains("### Tabla Pagos\nAlmacena liquidaciones instantáneas SPEI/PIX.\n\n### Tabla Usuarios"));
        assert!(updated.contains("Contiene las cuentas verificadas y perfiles KYC."));
        // El último módulo fue eliminado limpiamente sin dejar saltos triples ni rastro
        assert!(!updated.contains("## 3. Seguridad y Auditoría"));
        assert!(!updated.contains("Registro inmutable en bóveda"));
        assert!(!updated.contains("\n\n\n"));
        assert!(updated.ends_with("- `email`: TEXT\n"));

        // Verificar que la detección de solapamiento rechaza editar padre e hijo a la vez
        let overlapping_edits = vec![
            ModuleEdit {
                target_id: "2-base-de-datos".to_string(),
                new_content: "## 2. Base de Datos\nNuevo contenido.".to_string(),
            },
            ModuleEdit {
                target_id: "2-base-de-datos/tabla-pagos".to_string(),
                new_content: "### Tabla Pagos\nNuevo hijo.".to_string(),
            },
        ];
        assert!(replace_multiple_modules(SAMPLE_DOC, &overlapping_edits).is_err());
    }
}
