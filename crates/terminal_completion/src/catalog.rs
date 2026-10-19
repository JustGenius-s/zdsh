mod assets;
mod generators;

pub use generators::dynamic_command_signature_data;

pub use crate::metadata::*;
use assets::Assets;

pub fn signature_by_name(name: impl AsRef<str>) -> Option<Signature> {
    let file_path = format!("{}.json", name.as_ref());
    Assets::get(&file_path).and_then(|embedded_file| {
        let json_content = std::str::from_utf8(&embedded_file.data).ok()?;
        let fig_command: crate::metadata::fig_types::Command =
            serde_json::from_str(json_content).ok()?;
        let signatures: Vec<Signature> = fig_command.into();
        debug_assert!(
            signatures.len() <= 1,
            "Tried to fetch a signature by name for a signature that has multiple names"
        );
        signatures.into_iter().next()
    })
}

pub fn commands() -> Vec<Signature> {
    use itertools::Itertools;
    use rayon::prelude::*;

    Assets::iter()
        .collect_vec()
        .into_par_iter()
        .map(|path| Assets::get(&path))
        .filter_map(|embedded_file| {
            let embedded_data = embedded_file?.data;
            let json_content = std::str::from_utf8(&embedded_data).ok()?;
            let fig_command: crate::metadata::fig_types::Command =
                serde_json::from_str(json_content).ok()?;
            Some(Vec::from(fig_command))
        })
        .flatten()
        .collect()
}
