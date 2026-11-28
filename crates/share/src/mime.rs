//! Content types by extension; anything else is octet-stream.

pub fn mime_of(name: &str) -> &'static str {
    match crate::naming::extension_of(name).as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_table() {
        assert_eq!(super::mime_of("a.png"), "image/png");
        assert_eq!(super::mime_of("a.JPG"), "image/jpeg");
        assert_eq!(super::mime_of("a.jpeg"), "image/jpeg");
        assert_eq!(super::mime_of("a.webp"), "image/webp");
        assert_eq!(super::mime_of("a.gif"), "image/gif");
        assert_eq!(super::mime_of("a.mp4"), "video/mp4");
        assert_eq!(super::mime_of("a.webm"), "video/webm");
        assert_eq!(super::mime_of("a.mkv"), "video/x-matroska");
        assert_eq!(super::mime_of("a.mov"), "video/quicktime");
        assert_eq!(super::mime_of("a.avi"), "video/x-msvideo");
        assert_eq!(super::mime_of("a.bin"), "application/octet-stream");
        assert_eq!(super::mime_of("noext"), "application/octet-stream");
    }
}
