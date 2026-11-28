use std::ops::Range;

pub(super) fn payload(body: &[u8], content_type: Option<&str>) -> Option<Range<usize>> {
   let mut parts = parameters(content_type.unwrap_or_default());
   let range = if parts
      .next()
      .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"))
   {
      let boundary = parts.find_map(|part| parameter(part, "boundary"))?;
      multipart_payload(body, boundary)?
   } else {
      0..body.len()
   };
   (body[range.clone()]
      .iter()
      .find(|byte| !byte.is_ascii_whitespace())
      == Some(&b'{'))
   .then_some(range)
}

fn parameter<'a>(part: &'a str, name: &str) -> Option<&'a str> {
   let (key, value) = part.trim().split_once('=')?;
   let value = value.trim();
   key.trim().eq_ignore_ascii_case(name).then(|| {
      value
         .strip_prefix('"')
         .and_then(|quoted| quoted.strip_suffix('"'))
         .unwrap_or(value)
   })
}

fn parameters(value: &str) -> impl Iterator<Item = &str> {
   let mut quoted = false;
   let mut escaped = false;
   value.split(move |ch| {
      if escaped {
         escaped = false;
      } else if quoted && ch == '\\' {
         escaped = true;
      } else if ch == '"' {
         quoted = !quoted;
      }
      ch == ';' && !quoted
   })
}

fn multipart_payload(body: &[u8], boundary: &str) -> Option<Range<usize>> {
   if boundary.is_empty()
      || boundary.len() > 70
      || !boundary
         .bytes()
         .all(|byte| byte.is_ascii_graphic() || byte == b' ')
   {
      return None;
   }
   let marker = format!("\r\n--{boundary}");
   let marker = marker.as_bytes();
   let mut cursor = body.strip_prefix(&marker[2..]).map(|_| marker.len() - 2)?;
   let mut found = None;
   loop {
      let suffix = body.get(cursor..)?;
      if let Some(tail) = suffix.strip_prefix(b"--") {
         return (tail.is_empty() || tail.starts_with(b"\r\n"))
            .then_some(found)
            .flatten();
      }
      let headers = suffix.strip_prefix(b"\r\n")?;
      let header_end = headers.windows(4).position(|bytes| bytes == b"\r\n\r\n")?;
      let start = cursor + 2 + header_end + 4;
      let next = body[start..]
         .windows(marker.len())
         .enumerate()
         .find_map(|(offset, bytes)| {
            let end = start + offset + marker.len();
            (bytes == marker
               && body.get(end..).is_some_and(|tail| {
                  tail.starts_with(b"\r\n") || tail == b"--" || tail.starts_with(b"--\r\n")
               }))
            .then_some(start + offset)
         })?;
      let headers = std::str::from_utf8(&headers[..header_end]).ok()?;
      if headers.split("\r\n").any(is_payload_header) && found.replace(start..next).is_some() {
         return None;
      }
      cursor = next + marker.len();
   }
}

fn is_payload_header(header: &str) -> bool {
   let Some((name, value)) = header.split_once(':') else {
      return false;
   };
   if !name.eq_ignore_ascii_case("Content-Disposition") {
      return false;
   }
   let mut parts = parameters(value);
   parts
      .next()
      .is_some_and(|part| part.trim().eq_ignore_ascii_case("form-data"))
      && parts.any(|part| parameter(part, "name") == Some("payload_json"))
}

#[cfg(test)]
mod tests {
   use super::payload;

   #[test]
   fn finds_payload_json_without_decoding_the_file_bytes() {
      let body = b"--test\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"a; name=payload_json; b.txt\"\r\n\r\n\xff\r\n--test-like-file-data\r\n--test\r\nContent-Disposition: form-data; name=\"payload_json\"\r\n\r\n{\"content\":\"hello\"}\r\n--test--\r\n";
      let range = payload(body, Some("multipart/form-data; boundary=\"test\"")).unwrap();
      assert_eq!(&body[range], br#"{"content":"hello"}"#);
      assert!(payload(body, Some("multipart/form-data; boundary=wrong")).is_none());
   }
}
