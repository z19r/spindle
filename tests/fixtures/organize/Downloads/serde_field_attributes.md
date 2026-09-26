# serde field attributes worth remembering

- `#[serde(default)]` - missing field takes `Default`, not an error
- `#[serde(rename = "...")]` and `rename_all = "camelCase"`
- `#[serde(skip_serializing_if = "Option::is_none")]` keeps JSON small
- `#[serde(flatten)]` for a nested struct with no nesting in the wire
  format; it forces the untagged path, so it is slower
