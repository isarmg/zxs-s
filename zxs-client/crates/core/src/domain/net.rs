use std::net::IpAddr;

use super::{FileName, Result, invalid};

pub(crate) fn check_address(ip: IpAddr, allow_public: bool) -> Result<()> {
    let local = match ip {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (first == 100 && (64..=127).contains(&second))
        }
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => return check_address(v4.into(), allow_public),
            None => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
        },
    };
    if local || allow_public {
        Ok(())
    } else {
        Err(invalid(
            "public or wildcard HTTP address requires --allow-public-http",
        ))
    }
}

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
