use super::FileName;

#[must_use]
pub fn media_type(name: &FileName) -> &'static str {
    let extension = name
        .as_str()
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match extension.as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("pdf") => "application/pdf",
        Some("txt" | "md" | "log") => "text/plain; charset=utf-8",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}
