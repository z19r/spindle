use std::fmt::Write;

use crate::model::{Area, FileSummary};

use super::DescribeContext;

const DESCRIBE_RESPONSE_INSTRUCTIONS: &str = "Focus on the SUBJECT and THEME of the content, not the file format.\nA photo, video, PDF, and spreadsheet about the same topic should get similar tags.\n\nBe SPECIFIC enough that similar files can be told apart later:\n- Photos: say WHO is in the frame (how many people, adults/children, selfie vs posed vs candid), any pets and their species, the setting, and the activity or event. Two photos of the same person must get DIFFERENT descriptions when the companions, pets, location, or activity differ.\n- Documents: identify the document TYPE (contract, court filing, invoice, letter, medical record, ...), the parties or organizations involved, and any case numbers, matter names, account numbers, or dates. Two legal documents from different cases must be distinguishable from their summaries alone.\n- Screenshots: name the app or site shown and what is happening in it.\n- Nudity, sexual or intimate content: say so plainly and specifically in the summary (nude selfie, explicit photo of a couple, genital close-up, lingerie shot), tag it 'explicit' and 'private', and use category 'adult'. Never soften it into 'a person' or 'a selfie': the owner needs these kept apart from family and trip photos. Suggestive but clothed content gets the tag 'private' with its normal category.\n\nRespond in JSON:\n{\n  \"summary\": \"1-2 sentence description specific to THIS file's subject\",\n  \"tags\": [\"5-8 tags, most specific first (e.g. 'couple-photo', 'smith-v-jones', 'golden-retriever'), ending with general ones (e.g. 'pets', 'legal')\"],\n  \"suggested_category\": \"travel|nature|food|work|family|friends|people|selfies|kids|pets|home|sports|entertainment|music|gaming|art|memes|screenshots|receipts|science|tech|finance|health|education|events|vehicles|architecture|legal|adult|other\",\n  \"confidence\": 0.0-1.0\n}";

const GROUP_RESPONSE_INSTRUCTIONS: &str = "Respond in JSON:\n{\n  \"groups\": [\n    {\n      \"label\": \"Work/Acme Corp/Website Redesign\",\n      \"rationale\": \"Wireframes, mockups and copy for the 2024 site rebuild.\",\n      \"members\": [\n        { \"index\": 0 },\n        { \"index\": 3, \"dest_name\": \"mockups/home.png\" }\n      ]\n    }\n  ]\n}\n\nRules:\n- A file can only be in one group\n- Keep \"rationale\" to ONE short sentence saying what the group holds. It is shown in a narrow side panel, so nothing past a sentence is ever read: never pad it, and never repeat a phrase to fill space\n- EVERY file must be placed in exactly one group. Prefer groups of 2+ files. A file with no companions joins the nearest sub-folder you are already proposing (widen that folder's name if needed: a lone owner's manual joins \"Reference/Appliance Manuals\" and the folder becomes \"Reference/Manuals\"). It sits directly under the broad area ONLY when you propose no sub-folders in that area at all — never in a folder made only of its own name\n- NEVER use file-type words as folder names (PDFs, Documents, Images, Files, Misc, Other)\n- Prefer FEWER, FULLER groups. A folder earns its place by holding about four or more files; when the one you are about to propose would hold two or three, widen it until it does. Drafts, versions and revisions of one document stay together; the monthly bills or statements of one account share one folder rather than one folder per month; episodes of one show share one folder; photos of one person across different days, places and companions share one folder.\n- Never leave a lone file in a sub-folder of its own, and never leave a lone file at the bare area level beside sub-folders (\"Media\" holding one podcast next to \"Media/Podcasts/Rust Programming\" is wrong; it belongs in \"Media/Podcasts\")\n- The \"label\" CAN be a nested folder path using \"/\" to build a real directory tree, where each \"/\" becomes a subdirectory. TWO levels is the normal shape — a broad area and one subject inside it: \"Work/Acme Corp\", \"Finance/Taxes\", \"Legal/Smith v. Jones\", \"Photos/Hawaii Trip\".\n- When you do nest, go from general to specific: the top level is a broad area (Work, Photos, Finance, Legal, Personal), and the next narrows by client, project, year, event, or matter. A THIRD level needs a reason: propose one only when that deepest folder would itself hold four or more files, the way \"Work/Acme Corp/Website Redesign\" does when the rebuild has its own wireframes, mockups and copy. Never bury one or two files three levels down.\n- Name a folder for what is actually inside it rather than for a category: \"Smith v. Jones Lawsuit\" beats \"Legal Documents\", \"Dog Photos\" beats \"Personal Photos\". This is about NAMING a folder well, not about making more of them — a specific name on a folder of four files is the goal, a specific name on a folder of one is not.\n- Split a broad theme only when each side would fill a folder on its own. Different people, pets, cases, trips or events justify a split when there are enough files of each; two of one and three of another belong together under the wider name.
- The per-file descriptions you were given are deliberately fine-grained — they exist so you can tell similar files apart, NOT as a list of folders to create. Two photos described with different companions, pets or locations are still two photos of the same subject and usually belong in the same folder.\n- Use dest_name sub-paths to organize even further WITHIN a group (e.g. \"raw/beach.jpg\") when members share a group but differ in sub-subject\n- NEVER group by file type — group by subject, theme, or context\n- A .jpg, .mp4, .pdf, and .csv can all belong in the same group if they share a topic\n- dest_name is OPTIONAL. Omit it to keep the original filename, which is the usual case. Include it only to rename a file or to place it in a sub-path inside the group folder\n- Preserve source subfolder prefixes in dest_name ONLY when they add meaningful context\n- Drop misleading or redundant subfolder prefixes (e.g. a cat photo in \"porn/\" → just the filename)\n- Explicit or intimate content (tags 'explicit', 'private', 'sensitive') NEVER shares a folder with ordinary photos of the same people or places. Under a private area, sub-folders by person or occasion are fine.\n- When given, dest_name must end with the original file's name and extension\n";

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
    "You are organizing files into logical groups by TOPIC and THEME.\n     File type is IRRELEVANT — a photo, video, PDF, and spreadsheet about the same      subject belong in the same group.\n\n     Group files that share a common topic. Labels CAN be nested \"/\" paths to build a real folder tree (general → specific), usually two levels — for example:\n     - A vacation photo, a hotel receipt PDF, and a trip itinerary spreadsheet → \"Photos/Hawaii Trip\"\n     - A contract, invoices, and mockups for one client's project → \"Work/Acme Corp\"\n     - A movie clip, a fan art image, and a character guide PDF → \"Entertainment/Star Wars\"\n     - A presentation, meeting notes, and a project diagram → \"Work/Q4 Launch\"\n\n     {GROUP_RESPONSE_INSTRUCTIONS}"
  )
}

pub fn group_user_prompt(files: &[FileSummary]) -> String {
  let mut prompt = format!("Organize these {} files:\n", files.len());

  for file in files {
    let tags = file.description.tags.join(", ");
    let _ = write!(
      prompt,
      "[{}] {} — {} (tags: {})",
      file.index, file.source_path, file.description.summary, tags
    );
    if !file.metadata_hint.trim().is_empty() {
      let _ = write!(prompt, " [{}]", file.metadata_hint.trim());
    }
    prompt.push('\n');
  }

  prompt
}

/// Renames the user made in earlier reviews: the strongest signal for
/// what a label should be called.
pub fn group_corrections_note(
  renames: &[(String, String)],
) -> String {
  if renames.is_empty() {
    return String::new();
  }
  let mut note = String::from(
    "\nThe user has previously renamed folders you proposed. Use their \
     names, and never propose the old ones again:\n",
  );
  for (from, to) in renames {
    let _ = writeln!(note, "- \"{from}\" → \"{to}\"");
  }
  note.push_str(
    "A file marked [user filed under: X] was placed under X by the \
     user before; put it there again unless its content clearly changed.\n",
  );
  note
}

/// Hint listing folders previous runs already created. Empty when there is
/// no prior history. Goes in the user prompt (not the cached system block)
/// because the set of existing folders changes from run to run.
pub fn group_existing_groups_note(
  existing_labels: &[String],
) -> String {
  if existing_labels.is_empty() {
    return String::new();
  }
  let mut note = String::from(
    "\nThese folders already exist from previous runs. If a file clearly \
     belongs to one, REUSE its exact label as the group \"label\" so the \
     file joins that folder instead of creating a near-duplicate:\n",
  );
  for label in existing_labels {
    let _ = writeln!(note, "- {label}");
  }
  note
}

/// Rich context about existing groups: includes sample file
/// summaries/tags from each group so the model can match new files
/// against the actual content of the organized pool.
pub fn group_organized_context(
  groups: &[(String, Vec<crate::model::ContentDescription>)],
) -> String {
  if groups.is_empty() {
    return String::new();
  }
  let mut note = String::from(
    "\nThese folders already exist with the following contents. \
     Route new files into an existing group when the content clearly \
     fits — reuse its exact label:\n",
  );
  for (label, descriptions) in groups {
    note.push_str(&format!("\n## {label}\n"));
    for desc in descriptions.iter().take(5) {
      let tags = desc.tags.join(", ");
      let _ = std::fmt::Write::write_fmt(
        &mut note,
        format_args!("- {} (tags: {})\n", desc.summary, tags),
      );
    }
    if descriptions.len() > 5 {
      let _ = std::fmt::Write::write_fmt(
        &mut note,
        format_args!(
          "  ... and {} more files\n",
          descriptions.len() - 5
        ),
      );
    }
  }
  note
}

pub fn describe_text_user_prompt(
  context: &DescribeContext,
  excerpt: &str,
) -> String {
  let mut prompt = describe_user_prompt(context);
  prompt.push_str("\nContent excerpt (may be truncated):\n---\n");
  prompt.push_str(excerpt);
  prompt.push_str("\n---\n");
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

/// Stage-one prompt: assign every file to one top-level area.
pub fn route_system_prompt(areas: &[Area]) -> String {
  let mut prompt = String::from(
    "You are sorting files into a fixed set of top-level folders \
     (areas). Assign EVERY file to exactly one area by its index. \
     Judge by subject and purpose, never by file type. Files that \
     belong to one event, trip, project, pet, or matter MUST share an \
     area: a trip's photos, video, itinerary and hotel receipt go \
     together, as do a project's notes, mockups and screenshots. Look \
     across the whole list for such clusters before assigning. When a \
     file could fit two areas, pick the one a person would look in \
     first. A document addressed to the user as a CUSTOMER (a bill, \
     invoice to pay, statement, policy, receipt) belongs with money \
     matters no matter whose name is on it, even when that vendor \
     shares a name with a client or employer; work areas are for what \
     the user produces or receives as a worker. If an area exists for \
     private, intimate or explicit content, EVERY file tagged \
     explicit, sensitive or private, or whose summary mentions nudity \
     or sexual content, goes there, no matter who is in it or where it \
     was taken.\n\n\
     Areas:\n",
  );
  for area in areas {
    let _ = writeln!(prompt, "- {}: {}", area.name, area.description);
  }
  prompt.push_str(
    "\nRespond in JSON: {\"assignments\": [{\"index\": 0, \"area\": \"Work\"}, ...]}",
  );
  prompt
}

pub fn route_user_prompt(files: &[FileSummary]) -> String {
  let mut prompt = format!("Assign these {} files:\n", files.len());
  for file in files {
    let _ = writeln!(
      prompt,
      "[{}] {} — {}",
      file.index, file.filename, file.description.summary
    );
  }
  prompt
}

/// Stage-two constraint: every label lives under one area.
pub fn group_area_note(area: &Area) -> String {
  format!(
    "\nAll of these files belong to the top-level area \"{name}\" \
     ({desc}). Every group \"label\" MUST start with \"{name}/\" and \
     then name the sub-folder(s) beneath it. Do not repeat the area \
     name inside the sub-folder.\n",
    name = area.name,
    desc = area.description,
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::ContentDescription;
  use crate::model::DescriptionSource;

  #[test]
  fn route_prompts_list_areas_and_files() {
    let areas = vec![
      Area::new("Work", "jobs and clients"),
      Area::new("Finance", "taxes, bills"),
    ];
    let system = route_system_prompt(&areas);
    assert!(system.contains("- Work: jobs and clients"));
    assert!(system.contains("- Finance: taxes, bills"));
    assert!(system.contains("exactly one area"));
    assert!(system.contains("MUST share an area"));

    let files = vec![FileSummary {
      index: 4,
      filename: "w2.txt".to_string(),
      source_path: "Documents/w2.txt".to_string(),
      description: ContentDescription {
        summary: "2023 W-2 from Acme".to_string(),
        tags: vec![],
        suggested_category: "finance".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      },
      metadata_hint: String::new(),
    }];
    let user = route_user_prompt(&files);
    assert!(user.contains("[4] w2.txt — 2023 W-2 from Acme"));
  }

  #[test]
  fn user_prompt_shows_metadata_hint_when_present() {
    let mut f = FileSummary {
      index: 3,
      filename: "a.txt".to_string(),
      source_path: "a.txt".to_string(),
      description: ContentDescription {
        summary: "s".to_string(),
        tags: vec![],
        suggested_category: "other".to_string(),
        confidence: 0.9,
        source: DescriptionSource::Ai,
      },
      metadata_hint: String::new(),
    };
    assert!(
      !group_user_prompt(&[f.clone()]).contains("user filed under")
    );
    f.metadata_hint = "user filed under: Work/Acme".to_string();
    let line = group_user_prompt(&[f]);
    assert!(line.contains("[user filed under: Work/Acme]"), "{line}");
  }

  #[test]
  fn corrections_note_lists_renames() {
    assert!(group_corrections_note(&[]).is_empty());
    let note = group_corrections_note(&[(
      "Photos/Pets".to_string(),
      "Personal/Pets/Biscuit".to_string(),
    )]);
    assert!(
      note.contains("\"Photos/Pets\" → \"Personal/Pets/Biscuit\"")
    );
    assert!(note.contains("user filed under"));
  }

  #[test]
  fn group_rules_keep_lone_files_out_of_the_bare_area() {
    let p = group_system_prompt();
    assert!(p.contains("joins the nearest sub-folder"));
    assert!(p.contains("ONLY when you propose no sub-folders"));
    assert!(!p.contains("may sit directly under the broad area"));
  }

  #[test]
  fn route_prompt_sends_bills_to_money_even_from_a_client_name() {
    let areas =
      vec![Area::new("Work", "job"), Area::new("Finance", "money")];
    let p = route_system_prompt(&areas);
    assert!(p.contains("as a CUSTOMER"));
    assert!(p.contains("shares a name with a client"));
  }

  #[test]
  fn describe_instructions_name_explicit_content_plainly() {
    let p = describe_response_instructions();
    assert!(p.contains("tag it 'explicit' and 'private'"));
    assert!(p.contains("category 'adult'"));
    assert!(p.contains("|adult|"));
    assert!(p.contains("|selfies|") && p.contains("|screenshots|"));
  }

  #[test]
  fn route_and_group_rules_keep_private_content_apart() {
    let areas = vec![Area::new("Private", "intimate or explicit")];
    assert!(route_system_prompt(&areas)
      .contains("EVERY file tagged explicit, sensitive or private"));
    assert!(group_system_prompt()
      .contains("NEVER shares a folder with ordinary photos"));
  }

  #[test]
  fn area_note_demands_the_prefix() {
    let note =
      group_area_note(&Area::new("Legal", "contracts, leases"));
    assert!(note.contains("MUST start with \"Legal/\""));
    assert!(note.contains("contracts, leases"));
  }

  #[test]
  fn group_rules_forbid_omitting_files_and_type_words() {
    let system = group_system_prompt();
    assert!(!system.contains("can be omitted"));
    assert!(system.contains("EVERY file must be placed"));
    assert!(system.contains("NEVER use file-type words"));
    assert!(system.contains("FEWER, FULLER groups"));
    assert!(system.contains("one folder per month"));
  }

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
  fn existing_groups_note_is_empty_without_history() {
    assert!(group_existing_groups_note(&[]).is_empty());
  }

  #[test]
  fn existing_groups_note_lists_labels_and_asks_for_reuse() {
    let note = group_existing_groups_note(&[
      "Beach".to_string(),
      "Dogs".to_string(),
    ]);

    assert!(note.contains("Beach"));
    assert!(note.contains("Dogs"));
    assert!(note.to_lowercase().contains("reuse"));
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
          source: DescriptionSource::Ai,
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
          source: DescriptionSource::Ai,
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
          source: DescriptionSource::Ai,
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
          source: DescriptionSource::Ai,
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
        source: DescriptionSource::Ai,
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
          source: DescriptionSource::Ai,
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
        source: DescriptionSource::Ai,
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
        source: DescriptionSource::Ai,
      },
      metadata_hint: String::new(),
    }];

    let prompt = build_group_prompt(&files);

    assert!(prompt.contains("dest_name"));
    assert!(prompt.contains("\"members\""));
    assert!(prompt.contains("\"index\""));
  }

  #[test]
  fn describe_text_prompt_includes_excerpt_and_filename() {
    let ctx = DescribeContext {
      filename: "lease.pdf".to_string(),
      file_type_label: "PDF document".to_string(),
      file_size: 4096,
      metadata_hint: None,
    };

    let prompt = describe_text_user_prompt(
      &ctx,
      "LEASE AGREEMENT between Alice and Bob",
    );

    assert!(prompt.contains("lease.pdf"));
    assert!(prompt.contains("LEASE AGREEMENT between Alice and Bob"));
  }

  #[test]
  fn describe_instructions_demand_specific_distinctions() {
    let instructions = describe_response_instructions();

    assert!(instructions.contains("selfie"));
    assert!(instructions.contains("case numbers"));
    assert!(instructions.contains("5-8 tags"));
  }

  /// The granularity guidance used to argue with itself, and the
  /// splitting side carried all the examples: 58% of the groups on a
  /// 1226-file run held four files or fewer. Fullness is the rule
  /// now, and specificity is about what a folder is *called*.
  #[test]
  fn group_prompt_asks_for_fuller_groups_not_more_of_them() {
    let prompt = group_system_prompt();

    assert!(prompt.contains("Prefer FEWER, FULLER groups"));
    assert!(
      prompt.contains("four or more files"),
      "the fullness rule needs a number the model can apply"
    );
    assert!(
      prompt.contains("NAMING a folder well, not about making more"),
      "specificity must not read as a licence to split"
    );
    // The line that made depth the default, and its opposite.
    assert!(!prompt.contains("natural hierarchy exists"));
    assert!(prompt.contains("TWO levels is the normal shape"));
    assert!(prompt.contains("A THIRD level needs a reason"));
  }

  /// The describe prompt deliberately makes near-identical files
  /// describable apart. Grouping read that as a mandate to give each
  /// its own folder, so it is now told otherwise in as many words.
  #[test]
  fn group_prompt_says_fine_descriptions_are_not_folders() {
    let prompt = group_system_prompt();

    assert!(prompt.contains("NOT as a list of folders to create"));
    assert!(prompt.contains("tell similar files apart"));
  }

  #[test]
  fn organized_context_is_empty_without_groups() {
    assert!(group_organized_context(&[]).is_empty());
  }

  #[test]
  fn organized_context_includes_summaries_and_tags() {
    let groups = vec![(
      "Beach Photos".to_string(),
      vec![
        ContentDescription {
          summary: "Sandy beach at sunset".to_string(),
          tags: vec!["beach".to_string(), "sunset".to_string()],
          suggested_category: "travel".to_string(),
          confidence: 0.9,
          source: DescriptionSource::Ai,
        },
        ContentDescription {
          summary: "Ocean waves crashing".to_string(),
          tags: vec!["ocean".to_string(), "waves".to_string()],
          suggested_category: "nature".to_string(),
          confidence: 0.85,
          source: DescriptionSource::Ai,
        },
      ],
    )];

    let context = group_organized_context(&groups);

    assert!(context.contains("Beach Photos"));
    assert!(context.contains("Sandy beach at sunset"));
    assert!(context.contains("Ocean waves crashing"));
    assert!(context.contains("beach, sunset"));
    assert!(context.to_lowercase().contains("reuse"));
  }

  #[test]
  fn organized_context_truncates_beyond_five() {
    let descriptions: Vec<ContentDescription> = (0..8)
      .map(|i| ContentDescription {
        summary: format!("File {i}"),
        tags: vec![],
        suggested_category: "other".to_string(),
        confidence: 0.5,
        source: DescriptionSource::Ai,
      })
      .collect();
    let groups = vec![("Big Group".to_string(), descriptions)];

    let context = group_organized_context(&groups);

    assert!(context.contains("File 0"));
    assert!(context.contains("File 4"));
    assert!(!context.contains("File 5"));
    assert!(context.contains("3 more"));
  }
}
