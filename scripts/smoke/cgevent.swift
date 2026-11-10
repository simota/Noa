import Cocoa
import CoreGraphics

let args = CommandLine.arguments
let pid = pid_t(args[1])!
let cmd = args[2]

func windows() -> [(Int, CGRect)] {
    let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
    return list.compactMap { info in
        guard let owner = info[kCGWindowOwnerPID as String] as? Int, owner == Int(pid),
              let id = info[kCGWindowNumber as String] as? Int,
              let b = info[kCGWindowBounds as String] as? [String: CGFloat] else { return nil }
        return (id, CGRect(x: b["X"]!, y: b["Y"]!, width: b["Width"]!, height: b["Height"]!))
    }
}

func key(_ code: CGKeyCode, _ flags: CGEventFlags) {
    for down in [true, false] {
        let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)!
        e.flags = flags
        e.postToPid(pid)
        usleep(40_000)
    }
}

switch cmd {
case "windows":
    for (id, r) in windows() { print("\(id) \(Int(r.origin.x)) \(Int(r.origin.y)) \(Int(r.width)) \(Int(r.height))") }
case "key":
    let code = CGKeyCode(UInt16(args[3])!)
    var flags: CGEventFlags = []
    for m in args.dropFirst(4) {
        switch m { case "cmd": flags.insert(.maskCommand); case "shift": flags.insert(.maskShift); case "alt": flags.insert(.maskAlternate); case "ctrl": flags.insert(.maskControl); default: break }
    }
    key(code, flags)
case "type":
    for ch in args[3].unicodeScalars {
        let e = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true)!
        var u = [UniChar(ch.value)]
        e.keyboardSetUnicodeString(stringLength: 1, unicodeString: &u)
        e.postToPid(pid); usleep(20_000)
        let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false)!
        up.postToPid(pid); usleep(20_000)
    }
case "drag":
    let v = args.dropFirst(3).map { CGFloat(Double($0)!) }
    let from = CGPoint(x: v[0], y: v[1]), to = CGPoint(x: v[2], y: v[3])
    func mouse(_ t: CGEventType, _ p: CGPoint) {
        let e = CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)!
        e.postToPid(pid); usleep(30_000)
    }
    mouse(.mouseMoved, from); mouse(.leftMouseDown, from)
    for i in 1...10 {
        let f = CGFloat(i) / 10
        mouse(.leftMouseDragged, CGPoint(x: from.x + (to.x - from.x) * f, y: from.y + (to.y - from.y) * f))
    }
    mouse(.leftMouseUp, to)
case "click":
    let v = args.dropFirst(3).map { CGFloat(Double($0)!) }
    let p = CGPoint(x: v[0], y: v[1])
    for t in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
        let e = CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)!
        e.postToPid(pid); usleep(40_000)
    }
default: break
}
