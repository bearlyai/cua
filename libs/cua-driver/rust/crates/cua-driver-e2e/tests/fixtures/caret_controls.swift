import AppKit
import SwiftUI
let dir = CommandLine.arguments[1]
let sentinel = pid_t(CommandLine.arguments[2])!
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x:200,y:200,width:500,height:400), styleMask:[.titled,.closable,.resizable],backing:.buffered,defer:false)
window.title = "Caret fixture"
var fields: [String: NSTextField] = [:]
for (i, kind) in ["field", "search", "combo"].enumerated() {
 let field: NSTextField = kind == "search" ? NSSearchField() : kind == "combo" ? NSComboBox() : NSTextField()
 field.frame = NSRect(x:20,y:330-i*50,width:400,height:32)
 field.stringValue="Original"; field.identifier=NSUserInterfaceItemIdentifier(kind); field.setAccessibilityIdentifier(kind); field.setAccessibilityTitle(kind)
 window.contentView!.addSubview(field); fields[kind]=field
}
let rich=NSTextView(frame:NSRect(x:20,y:60,width:400,height:100)); rich.isRichText=true; rich.setAccessibilityIdentifier("rich"); rich.setAccessibilityTitle("rich")
rich.textStorage!.setAttributedString(NSAttributedString(string:"Original",attributes:[.font:NSFont.boldSystemFont(ofSize:16),.foregroundColor:NSColor.red])); window.contentView!.addSubview(rich)
class State: ObservableObject { @Published var text="Original" }
let state=State()
struct Field: View { @ObservedObject var state: State; var body: some View { TextField("swift",text:$state.text).accessibilityIdentifier("swift").accessibilityLabel("swift") } }
let host=NSHostingView(rootView:Field(state:state)); host.frame=NSRect(x:20,y:10,width:400,height:35); window.contentView!.addSubview(host)
window.orderFront(nil); window.makeFirstResponder(nil)
var last=""
var activations=0
var keyEvents=0
let keyObserver=NotificationCenter.default.addObserver(forName:NSWindow.didBecomeKeyNotification,object:window,queue:.main) { _ in keyEvents += 1 }
let activationObserver=NotificationCenter.default.addObserver(forName:NSApplication.didBecomeActiveNotification,object:app,queue:.main) { _ in activations += 1 }

func snapshot() {
 var result: [String:Any] = fields.mapValues{ $0.currentEditor()?.string ?? $0.stringValue }; result["rich"]=rich.string; result["swift"]=state.text
 result["active"]=app.isActive
 result["activations"]=activations
 result["key_events"]=keyEvents
 result["key"]=window.isKeyWindow
 result["rich_style_survived"] = (0..<rich.textStorage!.length).allSatisfy { i in
  if (rich.string as NSString).substring(with:NSRange(location:i,length:1)) == "!" { return true }
  guard let f=rich.textStorage!.attribute(.font,at:i,effectiveRange:nil) as? NSFont,let c=rich.textStorage!.attribute(.foregroundColor,at:i,effectiveRange:nil) as? NSColor else{return false}
  return NSFontManager.shared.traits(of:f).contains(.boldFontMask) && c == NSColor.red
 }

 result["rich_bold"] = rich.string.isEmpty ? false : NSFontManager.shared.traits(of:rich.textStorage!.attribute(.font,at:0,effectiveRange:nil) as! NSFont).contains(.boldFontMask)
 result["pointer"]=[NSEvent.mouseLocation.x,NSEvent.mouseLocation.y]
 result["front_pid"]=NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
 if let data=try? JSONSerialization.data(withJSONObject:result,options:[.sortedKeys]) { try? data.write(to:URL(fileURLWithPath:dir+"/state.json"),options:.atomic) }
}
let timer=Timer.scheduledTimer(withTimeInterval:0.02,repeats:true) { _ in
 if let cmd=try? String(contentsOfFile:dir+"/command",encoding:.utf8),cmd != last {
  last=cmd; let bits=cmd.trimmingCharacters(in:.whitespacesAndNewlines).split(separator:" ").map(String.init)
  if bits.first == "focus",bits.count>=4 {
   let kind=bits[1]; app.activate(ignoringOtherApps:true); window.makeKeyAndOrderFront(nil)
   if let f=fields[kind] { window.makeFirstResponder(f); f.currentEditor()?.selectedRange=NSRange(location:Int(bits[2])!,length:Int(bits[3])!) }
   else if kind == "swift" {
    func findField(_ view:NSView)->NSTextField? { if let f=view as? NSTextField { return f }; for v in view.subviews { if let f=findField(v) {return f} }; return nil }
    if let f=findField(host) {window.makeFirstResponder(f); f.currentEditor()?.selectedRange=NSRange(location:Int(bits[2])!,length:Int(bits[3])!)}
   }
   else if kind == "rich" { window.makeFirstResponder(rich); rich.setSelectedRange(NSRange(location:Int(bits[2])!,length:Int(bits[3])!)) }
  }
  if bits.first == "background" { NSRunningApplication(processIdentifier:sentinel)?.activate(options:[.activateIgnoringOtherApps]) }
  if bits.first == "empty" { window.makeFirstResponder(nil); if bits.count>1,let f=fields[bits[1]] {f.stringValue=""}; if bits.count>1 && bits[1]=="rich" {rich.string=""}; if bits.count>1 && bits[1]=="swift" {state.text="";host.rootView=Field(state:state);host.layoutSubtreeIfNeeded();host.displayIfNeeded()} }
  if bits.first == "reset" { for f in fields.values { f.stringValue="Original" }; rich.textStorage!.setAttributedString(NSAttributedString(string:"Original",attributes:[.font:NSFont.boldSystemFont(ofSize:16),.foregroundColor:NSColor.red])); state.text="Original";window.makeFirstResponder(nil) }
  if bits.first == "quit" { app.terminate(nil) }
 }
 snapshot()
}
try! String(ProcessInfo.processInfo.processIdentifier).write(toFile:dir+"/pid",atomically:true,encoding:.utf8)
app.run()
