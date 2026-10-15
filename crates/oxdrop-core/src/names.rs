//! Item Names: unique within a Transfer, and safe to save on an Android phone.

/// Extensions treated as one unit when numbering, so `a.tar.gz` becomes `a (2).tar.gz`.
const DOUBLE_EXTENSIONS: [&str; 5] = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz"];

/// Returns `desired`, or the first `name (n).ext` (n = 2, 3, …) that isn't `taken`;
/// a folder has no extension, so it becomes `name (n)`.
/// Comparison ignores case, because Android's shared storage does.
pub(crate) fn unique_name<'a>(
    desired: &str,
    is_folder: bool,
    taken: impl Iterator<Item = &'a str> + Clone,
) -> String {
    let is_taken = |candidate: &str| {
        let candidate = candidate.to_lowercase();
        taken.clone().any(|name| name.to_lowercase() == candidate)
    };
    if !is_taken(desired) {
        return desired.to_owned();
    }
    let (stem, extension) = if is_folder {
        (desired, "")
    } else {
        split_extension(desired)
    };
    (2..)
        .map(|n| format!("{stem} ({n}){extension}"))
        .find(|candidate| !is_taken(candidate))
        .expect("some number is free")
}

/// Splits `photo.jpg` into (`photo`, `.jpg`). A leading dot (`.env`) is not an extension.
fn split_extension(name: &str) -> (&str, &str) {
    let lower = name.to_lowercase();
    if let Some(double) = DOUBLE_EXTENSIONS.iter().find(|ext| lower.ends_with(*ext))
        && name.len() > double.len()
    {
        return name.split_at(name.len() - double.len());
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    }
}

/// The name to save as on the phone: characters Android's storage rejects
/// (`" * / : < > ? \ |` and control characters) become `_`.
/// Only for what the Receiver saves; the Sender always shows the real Item Name.
pub fn android_safe_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_safe_name_replaces_rejected_characters() {
        assert_eq!(
            android_safe_name("a:b?c*d\"e<f>g|h\\i"),
            "a_b_c_d_e_f_g_h_i"
        );
        assert_eq!(android_safe_name("tab\there"), "tab_here");
        assert_eq!(
            android_safe_name("Ảnh kỷ niệm 📷.png"),
            "Ảnh kỷ niệm 📷.png"
        );
    }

    #[test]
    fn splits_extensions_sensibly() {
        assert_eq!(split_extension("photo.jpg"), ("photo", ".jpg"));
        assert_eq!(split_extension("my.report.pdf"), ("my.report", ".pdf"));
        assert_eq!(split_extension("archive.TAR.GZ"), ("archive", ".TAR.GZ"));
        assert_eq!(split_extension(".tar.gz"), (".tar", ".gz"));
        assert_eq!(split_extension(".env"), (".env", ""));
        assert_eq!(split_extension("Makefile"), ("Makefile", ""));
    }
}
