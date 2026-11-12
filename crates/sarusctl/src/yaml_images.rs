//! Edit image scalars without reinterpreting the rest of Podman's YAML input.

use crate::AppError;
use saphyr::{YamlLoader, YamlOwned};
use saphyr_parser::{Event, Parser, ScalarStyle, Span, SpannedEventReceiver};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

enum Container {
    Mapping {
        key: bool,
        image: bool,
        enabled: bool,
    },
    Sequence {
        enabled: bool,
    },
}

impl Container {
    // Consume one complete child node. Collection children are consumed at their start event.
    fn child(&mut self, scalar: Option<&str>) -> (bool, bool) {
        match self {
            Self::Mapping {
                key,
                image,
                enabled,
            } => {
                if *key {
                    *key = false;
                    *image = scalar == Some("image");
                    (false, false) // The existing traversal does not visit complex mapping keys.
                } else {
                    *key = true;
                    (*enabled && *image, *enabled)
                }
            }
            Self::Sequence { enabled } => (false, *enabled),
        }
    }
}

struct Anchor {
    value: String,
    image: bool,
}

struct Alias {
    anchor: usize,
    range: Range<usize>,
    image: bool,
}

struct Edit {
    range: Range<usize>,
    replacement: String,
}

fn quoted(value: &str) -> String {
    // JSON string syntax is valid YAML double-quoted scalar syntax too.
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

// Event spans include node properties and, for quoted scalars, trailing comments.
// Narrow them to the scalar itself so those source bytes stay untouched.
fn scalar_range(
    source: &str,
    range: Range<usize>,
    style: ScalarStyle,
    properties: bool,
) -> Result<Range<usize>, AppError> {
    let raw = &source[range.clone()];
    let bytes = raw.as_bytes();
    let mut start = 0;
    if properties {
        loop {
            while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
                start += 1;
            }
            match bytes.get(start) {
                Some(b'#') => {
                    while bytes.get(start).is_some_and(|byte| *byte != b'\n') {
                        start += 1;
                    }
                }
                Some(b'!') if bytes.get(start + 1) == Some(&b'<') => {
                    while bytes.get(start).is_some_and(|byte| *byte != b'>') {
                        start += 1;
                    }
                    start += 1;
                }
                Some(b'&' | b'!') => {
                    while bytes
                        .get(start)
                        .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",[]{}".contains(byte))
                    {
                        start += 1;
                    }
                }
                _ => break,
            }
        }
    }
    let end = match style {
        ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted => {
            let quote = if style == ScalarStyle::SingleQuoted {
                b'\''
            } else {
                b'"'
            };
            let mut cursor = start + 1;
            loop {
                match bytes.get(cursor) {
                    Some(&byte) if byte == quote => {
                        if quote == b'\'' && bytes.get(cursor + 1) == Some(&quote) {
                            cursor += 2;
                        } else {
                            break cursor + 1;
                        }
                    }
                    Some(b'\\') if quote == b'"' => {
                        cursor += 2;
                    }
                    Some(_) => {
                        cursor += 1;
                    }
                    None => return Err(AppError::Yaml("cannot locate quoted image scalar".into())),
                }
            }
        }
        ScalarStyle::Literal | ScalarStyle::Folded => {
            // Unadorned block spans start at the body. With properties they start at the
            // tag/anchor, so skip the header as well. Preserve the separating whitespace.
            if properties {
                start += raw[start..]
                    .find('\n')
                    .ok_or_else(|| AppError::Yaml("cannot locate block image scalar".into()))?
                    + 1;
                while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
                    start += 1;
                }
            }
            raw.trim_end_matches(char::is_whitespace).len()
        }
        _ => raw.len(),
    };
    if start > end || end > raw.len() {
        return Err(AppError::Yaml("invalid image scalar source span".into()));
    }
    Ok(range.start + start..range.start + end)
}

struct ImageScalar {
    value: String,
    range: Range<usize>,
    style: ScalarStyle,
    properties: bool,
}

/// One parse provides both document inspection and later source-preserving image edits.
pub(super) struct ParsedManifest {
    source: String,
    pub(super) document: YamlOwned,
    scalars: Vec<ImageScalar>,
    anchors: BTreeMap<usize, Anchor>,
    aliases: Vec<Alias>,
}

impl ParsedManifest {
    pub(super) fn parse(source: String) -> Result<Self, AppError> {
        // Parser marks count characters, whereas string edits use UTF-8 byte offsets.
        let offsets: Vec<_> = source
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(source.len()))
            .collect();
        let byte_range = |span: Span| -> Result<Range<usize>, AppError> {
            match (
                offsets.get(span.start.index()),
                offsets.get(span.end.index()),
            ) {
                (Some(&start), Some(&end)) if start <= end => Ok(start..end),
                _ => Err(AppError::Yaml("invalid YAML source span".into())),
            }
        };
        let mut loader = YamlLoader::<YamlOwned>::default();
        let mut containers = Vec::new();
        let mut anchors = BTreeMap::<usize, Anchor>::new();
        let mut aliases = Vec::new();
        let mut scalars = Vec::new();
        let mut documents = 0;
        let mut complete_anchors = BTreeSet::new();
        let mut collection_anchors = Vec::new();

        for result in Parser::new_from_str(&source) {
            let (event, span) = result.map_err(|error| AppError::Yaml(error.to_string()))?;
            if matches!(event, Event::DocumentStart(..)) {
                documents += 1;
                if documents > 1 {
                    return Err(AppError::Yaml(
                        "YAML input must contain exactly one document".into(),
                    ));
                }
            }
            // Check scalar tags before the loader consumes them. BadValue is also its
            // internal empty-key marker, so an invalid key can disappear from the tree.
            match &event {
                Event::Scalar(value, style, _, tag) if tag.is_some() => {
                    if saphyr::Yaml::value_from_cow_and_metadata(
                        value.clone(),
                        *style,
                        tag.as_ref(),
                    )
                    .is_badvalue()
                    {
                        return Err(AppError::Yaml(format!(
                            "invalid YAML scalar value at line {} column {}",
                            span.start.line(),
                            span.start.col() + 1,
                        )));
                    }
                }
                Event::Alias(anchor) if !complete_anchors.contains(anchor) => {
                    return Err(AppError::Yaml(
                        "alias refers to an unfinished YAML anchor".into(),
                    ));
                }
                _ => {}
            }
            loader.on_event(event.clone(), span);
            if let Some(error) = loader.error() {
                return Err(AppError::Yaml(error.to_string()));
            }
            match event {
                Event::Scalar(value, style, anchor, tag) => {
                    let (image, _) = containers
                        .last_mut()
                        .map_or((false, true), |container: &mut Container| {
                            container.child(Some(&value))
                        });
                    if anchor != 0 {
                        complete_anchors.insert(anchor);
                        anchors.insert(
                            anchor,
                            Anchor {
                                value: value.to_string(),
                                image,
                            },
                        );
                    }
                    if image {
                        scalars.push(ImageScalar {
                            value: value.into_owned(),
                            range: byte_range(span)?,
                            style,
                            properties: anchor != 0 || tag.is_some(),
                        });
                    }
                }
                Event::Alias(anchor) => {
                    let value = anchors.get(&anchor).map(|anchor| anchor.value.as_str());
                    let (image, _) = containers
                        .last_mut()
                        .map_or((false, true), |container| container.child(value));
                    aliases.push(Alias {
                        anchor,
                        range: byte_range(span)?,
                        image,
                    });
                }
                Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                    collection_anchors.push(anchor);
                    let (_, enabled) = containers
                        .last_mut()
                        .map_or((false, true), |container| container.child(None));
                    containers.push(if matches!(event, Event::MappingStart(..)) {
                        Container::Mapping {
                            key: true,
                            image: false,
                            enabled,
                        }
                    } else {
                        Container::Sequence { enabled }
                    });
                }
                Event::MappingEnd | Event::SequenceEnd => {
                    containers.pop();
                    if let Some(anchor) = collection_anchors.pop().filter(|anchor| *anchor != 0) {
                        complete_anchors.insert(anchor);
                    }
                }
                _ => {}
            }
        }
        let mut documents = loader.into_documents();
        if documents.len() != 1 {
            return Err(AppError::Yaml(
                "YAML input must contain exactly one document".into(),
            ));
        }
        let document = documents.pop().unwrap();
        reject_bad_values(&document)?;
        Ok(Self {
            source,
            document,
            scalars,
            anchors,
            aliases,
        })
    }

    /// Only image scalars change; other scalar spellings remain for Podman to interpret.
    pub(super) fn render_images(
        &self,
        images: &BTreeMap<String, String>,
    ) -> Result<String, AppError> {
        let mut edits = Vec::new();
        for scalar in &self.scalars {
            if let Some(resolved) = images.get(&scalar.value) {
                let range = scalar_range(
                    &self.source,
                    scalar.range.clone(),
                    scalar.style,
                    scalar.properties,
                )?;
                let replacement =
                    if matches!(scalar.style, ScalarStyle::Literal | ScalarStyle::Folded) {
                        resolved.clone()
                    } else {
                        quoted(resolved)
                    };
                edits.push(Edit { range, replacement });
            }
        }
        for alias in &self.aliases {
            if let Some(anchor) = self.anchors.get(&alias.anchor) {
                let resolved = alias.image.then(|| images.get(&anchor.value)).flatten();
                if anchor.image && images.contains_key(&anchor.value) || resolved.is_some() {
                    // Keep non-image aliases at their original value when their anchor changes.
                    let value = resolved.unwrap_or(&anchor.value);
                    edits.push(Edit {
                        range: alias.range.clone(),
                        replacement: quoted(value),
                    });
                }
            }
        }
        edits.sort_by_key(|edit| edit.range.start);
        let mut rewritten = self.source.clone();
        let mut following = self.source.len();
        for edit in edits.into_iter().rev() {
            if edit.range.end > following {
                return Err(AppError::Yaml("overlapping image reference edits".into()));
            }
            following = edit.range.start;
            rewritten.replace_range(edit.range, &edit.replacement);
        }
        Ok(rewritten)
    }
}

// Saphyr can represent invalid explicit scalar tags as BadValue without a loader error.
// Reject those nodes instead of silently treating them as absent application fields.
fn reject_bad_values(document: &YamlOwned) -> Result<(), AppError> {
    let mut pending = vec![document];
    while let Some(node) = pending.pop() {
        match node {
            YamlOwned::BadValue => return Err(AppError::Yaml("invalid YAML scalar value".into())),
            YamlOwned::Mapping(mapping) => {
                for (key, value) in mapping {
                    pending.extend([key, value]);
                }
            }
            YamlOwned::Sequence(sequence) => pending.extend(sequence),
            YamlOwned::Tagged(_, value) => pending.push(value),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn replace_images(source: &str, images: &BTreeMap<String, String>) -> Result<String, AppError> {
        ParsedManifest::parse(source.to_owned())?.render_images(images)
    }

    const SOURCE: &str = "alpine:3.22";

    fn replacements() -> BTreeMap<String, String> {
        BTreeMap::from([(SOURCE.into(), format!("alpine@sha256:{}", "a".repeat(64)))])
    }

    #[test]
    fn parsed_manifest_supports_inspection_and_repeated_rendering() {
        let source = "metadata: {name: training-pod}\nspec:\n  securityContext: {runAsUser: 01000}\n  containers:\n    - name: app\n      image: &img alpine:3.22 # keep\nnote: *img\n";
        let parsed = ParsedManifest::parse(source.into()).unwrap();
        let original_document = parsed.document.clone();
        assert_eq!(
            crate::extract_images_from_yaml_value(&parsed.document).unwrap(),
            vec![SOURCE]
        );
        assert_eq!(
            crate::get_join_container_from_yaml_manifest(&parsed.document).unwrap(),
            "training-pod-app"
        );

        // Rendering uses the saved metadata and leaves the parsed source and tree reusable.
        for digit in ["a", "b"] {
            let digest = format!("alpine@sha256:{}", digit.repeat(64));
            let images = BTreeMap::from([(SOURCE.into(), digest.clone())]);
            let expected = source
                .replace("alpine:3.22 # keep", &format!("{} # keep", quoted(&digest)))
                .replace("note: *img", &format!("note: {}", quoted(SOURCE)));
            assert_eq!(parsed.render_images(&images).unwrap(), expected);
        }
        assert_eq!(parsed.render_images(&BTreeMap::new()).unwrap(), source);
        assert_eq!(parsed.document, original_document);
    }

    #[test]
    fn rejects_invalid_documents_and_scalar_tags() {
        for source in [
            "image: [",
            "image: first:tag\nimage: second:tag\n",
            "nested: {key: first, key: second}",
            "image: first:tag\n---\nimage: second:tag\n",
            "nested: {value: !!int invalid}",
            "nested: [!!bool invalid]",
            "? !!int invalid\n: value\n",
            "reference: *undefined",
            "reference: &self {*self : value}",
            "",
        ] {
            assert!(
                matches!(ParsedManifest::parse(source.into()), Err(AppError::Yaml(_))),
                "{source:?}"
            );
        }
    }

    #[test]
    fn retains_the_loaders_default_alias_expansion_limit() {
        let mut source = String::from("level0: &level0 [leaf, leaf, leaf]\n");
        for level in 1..12 {
            let previous = level - 1;
            source.push_str(&format!("level{level}: &level{level} [*level{previous}, *level{previous}, *level{previous}]\n"));
        }
        let error = match ParsedManifest::parse(source) {
            Ok(_) => panic!("excessive alias expansion was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("alias"), "{error}");
    }

    #[test]
    fn preserves_non_image_source_and_legacy_scalar_spellings() {
        let source = "# Grüße 🐧\r\nsecurityContext: {runAsUser: 01000, privileged: yes}\r\nmode: 0o755\r\nquoted: '01000'\r\ntext: |\r\n  image: alpine:3.22\r\ncontainers: [{name: app, image: alpine:3.22}] # keep comment\r\n";
        let images = replacements();
        let expected = source.replace(
            "image: alpine:3.22}]",
            &format!("image: {}}}]", quoted(&images[SOURCE])),
        );
        let rewritten = replace_images(source, &images).unwrap();
        assert_eq!(rewritten, expected);
        assert!(rewritten.contains("runAsUser: 01000"));
        assert!(rewritten.contains("quoted: '01000'"));
        assert!(ParsedManifest::parse(rewritten).is_ok());
    }

    #[test]
    fn replaces_quoted_flow_and_repeated_images_without_touching_similar_text() {
        let source = "note: alpine:3.22\ncontainers:\n  - image: 'alpine:3.22' # first\n  - {\"image\": \"alpine:3.22\", name: second}\nnested:\n  image: alpine:3.22\n  ignored: 42\n";
        let images = replacements();
        let digest = quoted(&images[SOURCE]);
        let expected = format!(
            "note: alpine:3.22\ncontainers:\n  - image: {digest} # first\n  - {{\"image\": {digest}, name: second}}\nnested:\n  image: {digest}\n  ignored: 42\n"
        );
        assert_eq!(replace_images(source, &images).unwrap(), expected);
    }

    #[test]
    fn preserves_block_headers_and_separating_newlines() {
        let images = replacements();
        for header in ["|-", ">-", "|- # comment"] {
            for newline in ["\n", "\r\n"] {
                let source = format!(
                    "image: {header}{newline}  {SOURCE}{newline}securityContext:{newline}  runAsUser: 01000{newline}"
                );
                let expected = source.replace(SOURCE, &images[SOURCE]);
                let rewritten = replace_images(&source, &images).unwrap();
                assert_eq!(rewritten, expected);
                let manifest = ParsedManifest::parse(rewritten).unwrap().document;
                assert_eq!(manifest["image"].as_str(), Some(images[SOURCE].as_str()));
            }
        }
        let source = "image: |-\n  alpine:3.22";
        assert_eq!(
            replace_images(source, &images).unwrap(),
            source.replace(SOURCE, &images[SOURCE])
        );
    }

    #[test]
    fn materializes_image_aliases_without_changing_their_definition() {
        let source =
            "reference: &img alpine:3.22\nnote: *img\ncontainers: [{image: *img}, {image: *img}]\n";
        let images = replacements();
        let expected = source.replace(
            "image: *img",
            &format!("image: {}", quoted(&images[SOURCE])),
        );
        assert_eq!(replace_images(source, &images).unwrap(), expected);
    }

    #[test]
    fn keeps_non_image_aliases_at_their_original_value() {
        let images = replacements();
        for scalar in [
            "&img alpine:3.22",
            "&img !!str 'alpine:3.22'",
            "!!str &img alpine:3.22",
            "&img |-\n  alpine:3.22",
        ] {
            let source = format!("image: {scalar}\nnote: *img\ncontainers: [{{image: *img}}]\n");
            let rewritten = replace_images(&source, &images).unwrap();
            let manifest = ParsedManifest::parse(rewritten.clone()).unwrap().document;
            assert_eq!(manifest["image"].as_str(), Some(images[SOURCE].as_str()));
            assert_eq!(
                manifest["containers"][0]["image"].as_str(),
                Some(images[SOURCE].as_str())
            );
            assert_eq!(manifest["note"].as_str(), Some(SOURCE));
            assert!(!rewritten.contains("*img"));
        }
    }

    #[test]
    fn rewrites_images_in_anchored_collections_and_with_aliased_keys() {
        let source = "key: &key image\ncontainers:\n  - &container {image: alpine:3.22}\n  - *container\n  - {*key : alpine:3.22}\n";
        let images = replacements();
        let rewritten = replace_images(source, &images).unwrap();
        let manifest = ParsedManifest::parse(rewritten).unwrap().document;
        for container in manifest["containers"].as_sequence().unwrap() {
            assert_eq!(container["image"].as_str(), Some(images[SOURCE].as_str()));
        }
    }

    #[test]
    fn preserves_unselected_values_and_rejects_malformed_yaml() {
        let source = "image: null\nnumeric: 01000\nnested: {image: another:tag}\n";
        assert_eq!(replace_images(source, &replacements()).unwrap(), source);
        assert!(matches!(
            replace_images("image: [", &replacements()),
            Err(AppError::Yaml(_))
        ));
    }
}
