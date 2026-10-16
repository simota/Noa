//! Deterministic mixed outline/bitmap faces built from the vendored test font.

use crate::face::FontData;

pub(super) fn mixed_color_face(color_chars: &[char]) -> FontData {
    let source = include_bytes!("../vendor/SymbolsNerdFontMono-Regular.ttf");
    let font = swash::FontRef::from_index(source, 0).unwrap();
    let heart = font.charmap().map('\u{f004}');
    let star = font.charmap().map('\u{f005}');
    assert_ne!(heart, 0);
    assert_ne!(star, 0);
    let mappings = [
        (' ', 1),
        ('M', heart),
        ('☀', star),
        ('❤', heart),
        ('🏽', star),
        ('👍', heart),
    ];
    let color_ids: Vec<_> = mappings
        .iter()
        .filter(|(ch, _)| color_chars.contains(ch))
        .map(|(_, id)| *id)
        .collect();
    let read_u16 = |offset| u16::from_be_bytes(source[offset..offset + 2].try_into().unwrap());
    let read_u32 =
        |offset| u32::from_be_bytes(source[offset..offset + 4].try_into().unwrap()) as usize;
    let mut tables = Vec::new();
    for i in 0..usize::from(read_u16(4)) {
        let record = 12 + i * 16;
        let tag: [u8; 4] = source[record..record + 4].try_into().unwrap();
        if tag == *b"cmap" {
            continue;
        }
        let start = read_u32(record + 8);
        tables.push((tag, source[start..start + read_u32(record + 12)].to_vec()));
    }
    let glyph_count = tables
        .iter()
        .find(|(tag, _)| tag == b"maxp")
        .map(|(_, data)| u16::from_be_bytes([data[4], data[5]]))
        .unwrap();

    // Format 12 cmap; glyph 1 is the vendored font's empty glyph, used for
    // HarfBuzz's replacement of default-ignorable variation selectors.
    let mut cmap = vec![0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12, 0, 12, 0, 0];
    cmap.extend(
        [16 + mappings.len() as u32 * 12, 0, mappings.len() as u32]
            .into_iter()
            .flat_map(u32::to_be_bytes),
    );
    for (ch, id) in mappings {
        cmap.extend(
            [ch as u32, ch as u32, u32::from(id)]
                .into_iter()
                .flat_map(u32::to_be_bytes),
        );
    }
    tables.push((*b"cmap", cmap));

    // One valid 8x8 RGBA red PNG, shared by the selected sbix glyphs.
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x08\x00\x00\x00\x08\x08\x06\x00\x00\x00\xc4\x0f\xbe\x8b\x00\x00\x00\x12IDAT\x78\x9c\x63\xf8\xcf\xc0\xf0\x1f\x1f\x66\x18\x19\x0a\x00\xc2\xd7\x7f\x81\x2f\x71\xe0\x01\x00\x00\x00\x00IEND\xae\x42\x60\x82";
    let mut strike = vec![0, 16, 0, 72];
    let mut images = Vec::new();
    let image_start = 4 + (u32::from(glyph_count) + 1) * 4;
    for id in 0..glyph_count {
        strike.extend((image_start + images.len() as u32).to_be_bytes());
        if color_ids.contains(&id) {
            images.extend_from_slice(b"\0\0\0\0png ");
            images.extend_from_slice(png);
        }
    }
    strike.extend((image_start + images.len() as u32).to_be_bytes());
    strike.extend(images);
    let mut sbix = vec![0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 12];
    sbix.extend(strike);
    tables.push((*b"sbix", sbix));
    tables.sort_by_key(|(tag, _)| *tag);

    let count = tables.len() as u16;
    let power = 1u16 << count.ilog2();
    let mut bytes = vec![0; 12 + tables.len() * 16];
    bytes[..4].copy_from_slice(&source[..4]);
    for (i, value) in [
        count,
        power * 16,
        count.ilog2() as u16,
        count * 16 - power * 16,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[4 + i * 2..6 + i * 2].copy_from_slice(&value.to_be_bytes());
    }
    let checksum = |data: &[u8]| {
        data.chunks(4).fold(0u32, |sum, chunk| {
            let mut word = [0; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    };
    let mut head = 0;
    for (i, (tag, mut data)) in tables.into_iter().enumerate() {
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        if tag == *b"head" {
            data[8..12].fill(0);
            head = bytes.len();
        }
        let record = 12 + i * 16;
        bytes[record..record + 4].copy_from_slice(&tag);
        for (j, value) in [checksum(&data), bytes.len() as u32, data.len() as u32]
            .into_iter()
            .enumerate()
        {
            bytes[record + 4 + j * 4..record + 8 + j * 4].copy_from_slice(&value.to_be_bytes());
        }
        bytes.extend(data);
    }
    let adjustment = 0xb1b0afbau32.wrapping_sub(checksum(&bytes));
    bytes[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
    FontData {
        bytes: bytes.into(),
        index: 0,
    }
}
