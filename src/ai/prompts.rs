use std::fmt::Write;

use crate::model::FileSummary;

use super::DescribeContext;

const DESCRIBE_RESPONSE_INSTRUCTIONS: &str = "Focus on the SUBJECT and THEME of the content, not the file format.\nA photo, video, PDF, and spreadsheet about the same topic should get similar tags.\n\nRespond in JSON:\n{\n  \"summary\": \"1-2 sentence description of the subject/theme of this content\",\n  \"tags\": [\"topic1\", \"topic2\", \"topic3\"],\n  \"suggested_category\": \"travel|nature|food|work|family|pets|sports|entertainment|art|science|tech|finance|health|education|events|vehicles|architecture|other\",\n  \"confidence\": 0.0-1.0\n}";

const GROUP_RESPONSE_INSTRUCTIONS: &str = "Respond in JSON:\n{\n  \"groups\": [\n    {\n      \"label\": \"...\",\n      \"rationale\": \"...\",\n      \"members\": [\n        { \"index\": 0, \"dest_name\": \"image1.jpg\" },\n        { \"index\": 3, \"dest_name\": \"porn/nude.jpg\" }\n      ]\n    }\n  ]\n}\n\nRules:\n- A file can only be in one group\n- Groups should have at least 2 members\n- Prefer fewer, larger groups over many tiny ones\n- Files that don't fit any group can be omitted\n- NEVER group by file type — group by subject, theme, or context\n- A .jpg, .mp4, .pdf, and .csv can all belong in the same group if they share a topic\n- For each member, dest_name is the filename or sub-path to use inside the group folder\n- Preserve source subfolder prefixes in dest_name ONLY when they add meaningful context\n- Drop misleading or redundant subfolder prefixes (e.g. a cat photo in \"porn/\" → just the filename)\n- dest_name must always end with the original file's name and extension\n";

pub fn describe_system_prompt() -> &'static str {
  "You are helping organize a messy folder. \
   Describe this file's content for organizational purposes."
}

pub fn describe_user_prompt(context: &DescribeContext) -> String {
  let mut prompt = format!(
    "File: {}\nType: {}\nSize: {} bytes\n",
    context.filename, context.file_type_label, context.file_size
  );

  if let Some(ref hint) = context.metadata_hint {
    prompt.push_str(&format!("Metadata: {hint}\n"));
  }

  prompt
}

pub fn describe_response_instructions() -> &'static str {
  DESCRIBE_RESPONSE_INSTRUCTIONS
}

pub fn group_system_prompt() -> String {
  format!(
    "You are organizing files into logical groups by TOPIC and THEME.\n     File type is IRRELEVANT — a photo, video, PDF, and spreadsheet about the same      subject belong in the same group.\n\n     Group files that share a common topic — for example:\n     - A vacation photo, a hotel receipt PDF, and a trip itinerary spreadsheet → \"Hawaii Trip\"\n     - A movie clip, a fan art image, and a character guide PDF → \"Star Wars\"\n     - A presentation, meeting notes, and a project diagram → \"Q4 Launch\"\n\n     {GROUP_RESPONSE_INSTRUCTIONS}"
  )
}

pub fn group_user_prompt(files: &[FileSummary]) -> String {
  let mut prompt = format!("Organize these {} files:\n", files.len());

  for file in files {
    let tags = file.description.tags.join(", ");
    let _ = writeln!(
      prompt,
      "[{}] {} — {} (tags: {})",
      file.index, file.source_path, file.description.summary, tags
    );
  }

  prompt
}

pub fn build_describe_prompt(context: &DescribeContext) -> String {
  format!(
    "{describe_system}\n\n{user}\n{instructions}",
    describe_system = describe_system_prompt(),
    user = describe_user_prompt(context),
    instructions = describe_response_instructions(),
  )
}

pub fn build_group_prompt(files: &[FileSummary]) -> String {
  format!(
    "{system}\n{user}",
    system = group_system_prompt(),
    user = group_user_prompt(files),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::ContentDescription;

  #[test]
  fn describe_prompt_includes_filename() {
    let ctx = DescribeContext {
      filename: "sunset_beach.jpg".to_string(),
      file_type_label: "JPEG image".to_string(),
      file_size: 4096,
      metadata_hint: None,
    };

    let prompt = build_describe_prompt(&ctx);

    assert!(prompt.contains("sunset_beach.jpg"));
  }

  #[test]
  fn describe_prompt_includes_file_type() {
    let ctx = DescribeContext {
      filename: "photo.png".to_string(),
      file_type_label: "PNG image".to_string(),
      file_size: 1024,
      metadata_hint: None,
    };

    let prompt = build_describe_prompt(&ctx);

    assert!(prompt.contains("PNG image"));
  }

  #[test]
  fn describe_prompt_includes_metadata_when_present() {
    let ctx = DescribeContext {
      filename: "trip.jpg".to_string(),
      file_type_label: "JPEG image".to_string(),
      file_size: 2048,
      metadata_hint: Some(
        "Taken 2024-06-15, iPhone 14 Pro".to_string(),
      ),
    };

    let prompt = build_describe_prompt(&ctx);

    assert!(prompt.contains("iPhone 14 Pro"));
  }

  #[test]
  fn describe_prompt_requests_json_response() {
    let ctx = DescribeContext {
      filename: "x.jpg".to_string(),
      file_type_label: "JPEG".to_string(),
      file_size: 100,
      metadata_hint: None,
    };

    let prompt = build_describe_prompt(&ctx);

    assert!(prompt.contains("JSON"));
    assert!(prompt.contains("summary"));
    assert!(prompt.contains("tags"));
    assert!(prompt.contains("suggested_category"));
    assert!(prompt.contains("confidence"));
  }

  #[test]
  fn group_prompt_includes_all_file_summaries() {
    let files = vec![
      FileSummary {
        index: 0,
        filename: "beach1.jpg".to_string(),
        source_path: "beach1.jpg".to_string(),
        description: ContentDescription {
          summary: "Sandy beach at sunset".to_string(),
          tags: vec!["beach".to_string(), "sunset".to_string()],
          suggested_category: "photo".to_string(),
          confidence: 0.9,
        },
        metadata_hint: String::new(),
      },
      FileSummary {
        index: 1,
        filename: "beach2.jpg".to_string(),
        source_path: "beach2.jpg".to_string(),
        description: ContentDescription {
          summary: "Ocean waves on shore".to_string(),
          tags: vec!["beach".to_string(), "ocean".to_string()],
          suggested_category: "photo".to_string(),
          confidence: 0.85,
        },
        metadata_hint: String::new(),
      },
    ];

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("beach1.jpg"));
    assert!(prompt.contains("beach2.jpg"));
    assert!(prompt.contains("Sandy beach at sunset"));
    assert!(prompt.contains("Ocean waves on shore"));
  }

  #[test]
  fn group_prompt_includes_file_indices() {
    let files = vec![
      FileSummary {
        index: 0,
        filename: "a.jpg".to_string(),
        source_path: "a.jpg".to_string(),
        description: ContentDescription {
          summary: "A thing".to_string(),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.8,
        },
        metadata_hint: String::new(),
      },
      FileSummary {
        index: 1,
        filename: "b.jpg".to_string(),
        source_path: "b.jpg".to_string(),
        description: ContentDescription {
          summary: "B thing".to_string(),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.8,
        },
        metadata_hint: String::new(),
      },
    ];

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("[0]"));
    assert!(prompt.contains("[1]"));
  }

  #[test]
  fn group_prompt_requests_json_with_groups_array() {
    let files = vec![FileSummary {
      index: 0,
      filename: "x.jpg".to_string(),
      source_path: "x.jpg".to_string(),
      description: ContentDescription {
        summary: "X".to_string(),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.8,
      },
      metadata_hint: String::new(),
    }];

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("JSON"));
    assert!(prompt.contains("groups"));
    assert!(prompt.contains("label"));
    assert!(prompt.contains("rationale"));
    assert!(prompt.contains("dest_name"));
  }

  #[test]
  fn group_prompt_includes_file_count() {
    let files: Vec<FileSummary> = (0..5)
      .map(|i| FileSummary {
        index: i,
        filename: format!("file{i}.jpg"),
        source_path: format!("file{i}.jpg"),
        description: ContentDescription {
          summary: format!("File {i}"),
          tags: vec![],
          suggested_category: "photo".to_string(),
          confidence: 0.8,
        },
        metadata_hint: String::new(),
      })
      .collect();

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("5"));
  }

  #[test]
  fn group_prompt_shows_source_paths() {
    let files = vec![FileSummary {
      index: 0,
      filename: "image3.jpg".to_string(),
      source_path: "porn/image3.jpg".to_string(),
      description: ContentDescription {
        summary: "A cat photo".to_string(),
        tags: vec!["cat".to_string()],
        suggested_category: "pets".to_string(),
        confidence: 0.9,
      },
      metadata_hint: String::new(),
    }];

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("porn/image3.jpg"));
  }

  #[test]
  fn group_prompt_requests_dest_name_in_response() {
    let files = vec![FileSummary {
      index: 0,
      filename: "x.jpg".to_string(),
      source_path: "x.jpg".to_string(),
      description: ContentDescription {
        summary: "X".to_string(),
        tags: vec![],
        suggested_category: "photo".to_string(),
        confidence: 0.8,
      },
      metadata_hint: String::new(),
    }];

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("dest_name"));
    assert!(prompt.contains("\"members\""));
    assert!(prompt.contains("\"index\""));
  }
}
