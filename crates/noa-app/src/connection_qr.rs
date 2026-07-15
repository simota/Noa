use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use qrcode::{Color, QrCode};
use serde::{Deserialize, Serialize};

const PAYLOAD_TYPE: &str = "noa.remote-connection";
const PAYLOAD_VERSION: u8 = 1;
const QUIET_ZONE_MODULES: usize = 4;
const MODULE_PIXELS: usize = 8;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteConnectionPayload {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) version: u8,
    pub(crate) url: String,
    pub(crate) token: String,
}

impl RemoteConnectionPayload {
    pub(crate) fn new(bind_addr: IpAddr, port: u16, token: String) -> io::Result<Self> {
        if token.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "server token is empty",
            ));
        }
        let advertised = advertised_ip(bind_addr, discover_lan_ip(bind_addr));
        let address = SocketAddr::new(advertised?, port);
        Ok(Self {
            kind: PAYLOAD_TYPE.to_string(),
            version: PAYLOAD_VERSION,
            url: format!("ws://{address}"),
            token,
        })
    }

    pub(crate) fn to_canonical_json(&self) -> io::Result<String> {
        serde_json::to_string(self).map_err(io::Error::other)
    }
}

fn advertised_ip(bind_addr: IpAddr, discovered: Option<IpAddr>) -> io::Result<IpAddr> {
    if !bind_addr.is_unspecified() {
        return Ok(bind_addr);
    }
    discovered
        .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
        .ok_or_else(|| {
            io::Error::other(
                "Could not determine a reachable LAN address. Set Server Bind to this Mac's LAN IP or use a protected tunnel, then try again.",
            )
        })
}

fn discover_lan_ip(bind_addr: IpAddr) -> Option<IpAddr> {
    let (local, route) = match bind_addr {
        IpAddr::V4(_) => (
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9),
        ),
        IpAddr::V6(_) => (
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
            SocketAddr::new(IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().ok()?), 9),
        ),
    };
    let socket = UdpSocket::bind(local).ok()?;
    socket.connect(route).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

pub(crate) fn render_png(payload: &RemoteConnectionPayload) -> io::Result<Vec<u8>> {
    let json = payload.to_canonical_json()?;
    let qr = QrCode::new(json.as_bytes()).map_err(io::Error::other)?;
    let modules = qr.width();
    let image_modules = modules + QUIET_ZONE_MODULES * 2;
    let size = image_modules * MODULE_PIXELS;
    let mut pixels = vec![255_u8; size * size];
    for y in 0..modules {
        for x in 0..modules {
            if qr[(x, y)] != Color::Dark {
                continue;
            }
            let start_x = (x + QUIET_ZONE_MODULES) * MODULE_PIXELS;
            let start_y = (y + QUIET_ZONE_MODULES) * MODULE_PIXELS;
            for py in start_y..start_y + MODULE_PIXELS {
                pixels[py * size + start_x..py * size + start_x + MODULE_PIXELS].fill(0);
            }
        }
    }

    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, size as u32, size as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(io::Error::other)?;
        writer.write_image_data(&pixels).map_err(io::Error::other)?;
    }
    Ok(png)
}

#[cfg(target_os = "macos")]
pub(crate) fn show_pairing_qr(url: &str, png: &[u8]) -> io::Result<()> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSRect, NSString};

    let alert_class =
        AnyClass::get(c"NSAlert").ok_or_else(|| io::Error::other("NSAlert is unavailable"))?;
    let data_class =
        AnyClass::get(c"NSData").ok_or_else(|| io::Error::other("NSData is unavailable"))?;
    let image_class =
        AnyClass::get(c"NSImage").ok_or_else(|| io::Error::other("NSImage is unavailable"))?;
    let image_view_class = AnyClass::get(c"NSImageView")
        .ok_or_else(|| io::Error::other("NSImageView is unavailable"))?;
    let title = NSString::from_str("Connect Noa Remote");
    let info = NSString::from_str(url);
    let button = NSString::from_str("Done");

    // SAFETY: all selectors and argument types match their AppKit/Foundation APIs;
    // the modal owns its accessory view for the duration of runModal.
    unsafe {
        let alert: *mut AnyObject = msg_send![alert_class, new];
        let _: () = msg_send![alert, setMessageText: &*title];
        let _: () = msg_send![alert, setInformativeText: &*info];
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*button];

        let data: *mut AnyObject =
            msg_send![data_class, dataWithBytes: png.as_ptr(), length: png.len()];
        let image_alloc: *mut AnyObject = msg_send![image_class, alloc];
        let image: *mut AnyObject = msg_send![image_alloc, initWithData: data];
        if image.is_null() {
            let _: () = msg_send![alert, release];
            return Err(io::Error::other("could not create the QR image"));
        }
        let natural_size = png
            .get(16..20)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
            .unwrap_or(360) as f64;
        let frame = NSRect::new(
            objc2_foundation::NSPoint::new(0.0, 0.0),
            objc2_foundation::NSSize::new(natural_size, natural_size),
        );
        let view_alloc: *mut AnyObject = msg_send![image_view_class, alloc];
        let image_view: *mut AnyObject = msg_send![view_alloc, initWithFrame: frame];
        let _: () = msg_send![image_view, setImage: image];
        let _: () = msg_send![image_view, setImageScaling: 3_isize];
        let _: () = msg_send![alert, setAccessoryView: image_view];
        let _: isize = msg_send![alert, runModal];
        let _: () = msg_send![image_view, release];
        let _: () = msg_send![image, release];
        let _: () = msg_send![alert, release];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn show_pairing_qr(_url: &str, _png: &[u8]) -> io::Result<()> {
    Err(io::Error::other(
        "Remote App QR display is available on macOS only",
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn show_pairing_error(message: &str) -> io::Result<()> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    let alert_class =
        AnyClass::get(c"NSAlert").ok_or_else(|| io::Error::other("NSAlert is unavailable"))?;
    let title = NSString::from_str("Could Not Create Remote App QR");
    let info = NSString::from_str(message);
    let button = NSString::from_str("OK");
    // SAFETY: selectors and argument types match NSAlert's AppKit API.
    unsafe {
        let alert: *mut AnyObject = msg_send![alert_class, new];
        let _: () = msg_send![alert, setMessageText: &*title];
        let _: () = msg_send![alert, setInformativeText: &*info];
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*button];
        let _: isize = msg_send![alert, runModal];
        let _: () = msg_send![alert, release];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn show_pairing_error(_message: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_with_canonical_field_order() {
        let payload = RemoteConnectionPayload::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            61771,
            "secret".to_string(),
        )
        .unwrap();
        let json = payload.to_canonical_json().unwrap();
        assert_eq!(
            json,
            r#"{"type":"noa.remote-connection","version":1,"url":"ws://192.168.1.20:61771","token":"secret"}"#
        );
        let decoded = serde_json::from_str::<RemoteConnectionPayload>(&json).unwrap();
        assert!(decoded == payload);
    }

    #[test]
    fn ipv6_url_uses_brackets() {
        let payload = RemoteConnectionPayload::new(
            "2001:db8::2".parse().unwrap(),
            61771,
            "secret".to_string(),
        )
        .unwrap();
        assert_eq!(payload.url, "ws://[2001:db8::2]:61771");
    }

    #[test]
    fn wildcard_uses_discovered_lan_address() {
        let ip = advertised_ip(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8))),
        )
        .unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)));
    }

    #[test]
    fn wildcard_rejects_missing_or_loopback_discovery() {
        let error = advertised_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED), None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Server Bind"));
        assert!(message.contains("protected tunnel"));
        assert!(
            advertised_ip(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
            )
            .is_err()
        );
    }

    #[test]
    fn empty_token_is_rejected() {
        assert!(
            RemoteConnectionPayload::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 61771, String::new())
                .is_err()
        );
    }

    #[test]
    fn png_has_a_white_quiet_zone() {
        let payload = RemoteConnectionPayload::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            61771,
            "secret".to_string(),
        )
        .unwrap();
        let bytes = render_png(&payload).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::IDENTITY);
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        let pixels = &pixels[..info.buffer_size()];
        let width = info.width as usize;
        let quiet_pixels = QUIET_ZONE_MODULES * MODULE_PIXELS;
        assert!(
            pixels[..quiet_pixels * width]
                .iter()
                .all(|pixel| *pixel == 255)
        );
        assert!(
            pixels
                .chunks_exact(width)
                .all(|row| row[..quiet_pixels].iter().all(|pixel| *pixel == 255))
        );
        assert!(pixels.contains(&0));
    }
}
