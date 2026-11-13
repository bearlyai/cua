// WinForms fixture for the Windows foreground liveness proof.
//
// One window per process. The proof drives it through a command file
// (<Name>-cmd.txt, "seq op args...") and reads its state from <Name>.json,
// written every timer tick. Every input message the window's thread pumps
// (mouse buttons, keys, characters, wheel) is recorded with the control it
// was addressed to, so a row can show exactly which control received what.
//
// Written for the C# 5 compiler that Windows PowerShell's Add-Type uses.
using System;
using System.Collections.Generic;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows.Automation;
using System.Windows.Automation.Provider;
using System.Windows.Forms;

namespace BearlyLiveness
{
    public static class Native
    {
        [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr hwnd);
        [DllImport("user32.dll")] public static extern IntPtr GetFocus();
        [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
        [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hwnd);
        [DllImport("user32.dll")] public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
        [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint attach, uint to, bool on);
        [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern bool MoveFileEx(string from, string to, int flags);
    }

    public class Recorder : IMessageFilter
    {
        public Fixture Owner;

        public bool PreFilterMessage(ref Message m)
        {
            string kind = null;
            long value = 0;
            long wp = m.WParam.ToInt64();
            switch (m.Msg)
            {
                case 0x100: kind = "keydown"; value = wp & 0xFFFF; break;
                case 0x101: kind = "keyup"; value = wp & 0xFFFF; break;
                case 0x102: kind = "char"; value = wp & 0xFFFF; break;
                case 0x104: kind = "syskeydown"; value = wp & 0xFFFF; break;
                case 0x105: kind = "syskeyup"; value = wp & 0xFFFF; break;
                case 0x106: kind = "syschar"; value = wp & 0xFFFF; break;
                case 0x201: kind = "ldown"; break;
                case 0x202: kind = "lup"; break;
                case 0x203: kind = "ldbl"; break;
                case 0x204: kind = "rdown"; break;
                case 0x205: kind = "rup"; break;
                case 0x206: kind = "rdbl"; break;
                case 0x207: kind = "mdown"; break;
                case 0x208: kind = "mup"; break;
                case 0x20A: kind = "wheel"; value = (short)((wp >> 16) & 0xFFFF); break;
                case 0x20E: kind = "hwheel"; value = (short)((wp >> 16) & 0xFFFF); break;
            }
            if (kind != null && Owner != null)
            {
                Owner.Record(m.HWnd, kind, value);
            }
            return false;
        }
    }

    // A text box whose focus behaviour the proof chooses:
    //   normal    - an ordinary text box;
    //   bounce    - never keeps focus: every WM_SETFOCUS is passed straight on
    //               to the focus sink, so keyboard focus is never reported;
    //   clickonly - rejects focus (bounces) unless a mouse button went down on
    //               it in the last second, so SetFocus fails and a click works;
    //   clickaway - bounces, and a mouse button going down on it brings the
    //               window named by AwayTo to the front, so a click on it
    //               completes with another window in front.
    public class FocusBox : TextBox
    {
        public string Mode = "normal";
        public Control Sink;
        public IntPtr AwayTo = IntPtr.Zero;
        public int LastDown = 0;
        public bool HadDown = false;
        public int Bounces = 0;

        bool Bounce()
        {
            if (Mode == "bounce" || Mode == "clickaway") return true;
            if (Mode == "clickonly") return !HadDown || unchecked(Environment.TickCount - LastDown) > 1000;
            return false;
        }

        protected override void WndProc(ref Message m)
        {
            if (m.Msg == 0x201 || m.Msg == 0x203)
            {
                LastDown = Environment.TickCount;
                HadDown = true;
                if (Mode == "clickaway" && AwayTo != IntPtr.Zero)
                {
                    Native.SetForegroundWindow(AwayTo);
                }
            }
            if (m.Msg == 0x7 && Bounce())
            {
                Bounces++;
                if (Sink != null && Sink.IsHandleCreated)
                {
                    Native.SetFocus(Sink.Handle);
                }
                return;
            }
            base.WndProc(ref m);
        }
    }

    // A control with its own UI Automation provider: a List with ScrollPattern
    // whose Scroll calls succeed or fail as the proof chooses.
    public class CustomScroller : Control
    {
        public string Mode = "ok";
        public int Calls = 0;
        public int VPos = 0;
        public int HPos = 0;
        public bool Gone = false;
        public string LastError = "";
        public Action OnGone;
        public Action<string, long> OnCall;
        public bool PercentFails { get { return Mode.EndsWith("-nopercent"); } }
        public bool HPercentFails { get { return Mode.EndsWith("-nohpercent"); } }
        public bool VMinusOne { get { return Mode.StartsWith("vminus1"); } }
        // Takes the foreground from inside Scroll; the argument closes the spare
        // window first. Returns how the foreground was taken (0 = it was not).
        public Func<bool, long> Steal;

        public void Mark(string kind, long value)
        {
            if (OnCall != null) OnCall(kind, value);
        }
        public int UiThread;
        ScrollerProvider provider;

        public CustomScroller()
        {
            BackColor = Color.White;
        }

        protected override void WndProc(ref Message m)
        {
            if (m.Msg == 0x3D && unchecked((int)m.LParam.ToInt64()) == AutomationInteropProvider.RootObjectId)
            {
                if (provider == null)
                {
                    provider = new ScrollerProvider(this, Handle, AccessibleName);
                }
                m.Result = AutomationInteropProvider.ReturnRawElementProvider(Handle, m.WParam, m.LParam, provider);
                return;
            }
            base.WndProc(ref m);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            base.OnPaint(e);
            e.Graphics.DrawString("v " + VPos + " h " + HPos + " calls " + Calls, Font, Brushes.Black, 4, 4);
        }

        static int Delta(ScrollAmount amount)
        {
            if (amount == ScrollAmount.LargeIncrement || amount == ScrollAmount.SmallIncrement) return 1;
            if (amount == ScrollAmount.LargeDecrement || amount == ScrollAmount.SmallDecrement) return -1;
            return 0;
        }

        public void ScrollCall(ScrollAmount horizontal, ScrollAmount vertical)
        {
            bool remove = false;
            bool steal = false;
            try
            {
                lock (this)
                {
                    if (OnCall != null) OnCall(System.Threading.Thread.CurrentThread.ManagedThreadId == UiThread ? "scrollcall-ui" : "scrollcall-other", Calls + 1);
                    if (Gone) throw new ElementNotAvailableException();
                    Calls++;
                    int dv = Delta(vertical);
                    int dh = Delta(horizontal);
                    if (Mode == "fail-first" && Calls == 1)
                    {
                        LastError = "rejected first call";
                        throw new InvalidOperationException("fixture: first scroll rejected, nothing moved");
                    }
                    if (Mode.StartsWith("move-then-fail") && Calls == 1)
                    {
                        VPos += dv;
                        HPos += dh;
                        LastError = "moved then failed";
                        throw new InvalidOperationException("fixture: moved, then failed");
                    }
                    if (Mode.StartsWith("fail-after-"))
                    {
                        int n = int.Parse(Mode.Substring(11));
                        if (Calls > n)
                        {
                            LastError = "failed after " + n;
                            throw new InvalidOperationException("fixture: scroll failed after " + n + " step(s)");
                        }
                    }
                    if (Mode == "move-then-remove" && Calls == 1)
                    {
                        VPos += dv;
                        HPos += dh;
                        Gone = true;
                        remove = true;
                        LastError = "moved, then removed";
                        throw new ElementNotAvailableException();
                    }
                    if (Mode.StartsWith("remove-after-"))
                    {
                        int n = int.Parse(Mode.Substring(13));
                        if (Calls > n)
                        {
                            Gone = true;
                            remove = true;
                            LastError = "removed after " + n;
                            throw new ElementNotAvailableException();
                        }
                    }
                    VPos += dv;
                    HPos += dh;
                    steal = Mode.StartsWith("steal") && Calls == 1;
                }
                // Outside the lock: closing the spare window runs on the UI thread.
                if (steal && Steal != null)
                {
                    Mark("steal", Steal(Mode == "steal-close-spare"));
                }
            }
            finally
            {
                if (remove && OnGone != null && IsHandleCreated)
                {
                    BeginInvoke(new MethodInvoker(delegate { OnGone(); }));
                }
                if (IsHandleCreated)
                {
                    BeginInvoke(new MethodInvoker(delegate { if (!IsDisposed) Invalidate(); }));
                }
            }
        }
    }

    public class ScrollerProvider : IRawElementProviderSimple, IScrollProvider
    {
        readonly CustomScroller owner;
        readonly IntPtr hwnd;
        readonly string name;

        public ScrollerProvider(CustomScroller owner, IntPtr hwnd, string name)
        {
            this.owner = owner;
            this.hwnd = hwnd;
            this.name = name;
        }

        public ProviderOptions ProviderOptions { get { return ProviderOptions.ServerSideProvider; } }

        public IRawElementProviderSimple HostRawElementProvider
        {
            get { return AutomationInteropProvider.HostProviderFromHandle(hwnd); }
        }

        public object GetPatternProvider(int patternId)
        {
            owner.Mark("getpattern", patternId);
            if (owner.Gone) throw new ElementNotAvailableException();
            if (patternId == ScrollPatternIdentifiers.Pattern.Id) return this;
            return null;
        }

        public object GetPropertyValue(int propertyId)
        {
            if (propertyId == AutomationElementIdentifiers.NameProperty.Id) return name;
            if (propertyId == AutomationElementIdentifiers.ControlTypeProperty.Id) return ControlType.List.Id;
            if (propertyId == AutomationElementIdentifiers.AutomationIdProperty.Id) return "custom-scroller";
            if (propertyId == AutomationElementIdentifiers.IsControlElementProperty.Id) return true;
            if (propertyId == AutomationElementIdentifiers.IsContentElementProperty.Id) return true;
            return null;
        }

        public void Scroll(ScrollAmount horizontalAmount, ScrollAmount verticalAmount)
        {
            owner.ScrollCall(horizontalAmount, verticalAmount);
        }

        public void SetScrollPercent(double horizontalPercent, double verticalPercent) { }
        // Percents really move with the content (50 is the start), unless the mode
        // makes the getters themselves fail.
        double Percent(string axis, int pos)
        {
            if (owner.Gone)
            {
                owner.Mark("percent-gone-" + axis, pos);
                throw new ElementNotAvailableException();
            }
            if (axis == "v" && owner.VMinusOne)
            {
                owner.Mark("percent-minus1-v", pos);
                return -1;
            }
            if (owner.PercentFails || (axis == "h" && owner.HPercentFails))
            {
                owner.Mark("percent-fail-" + axis, pos);
                throw new InvalidOperationException("fixture: scroll percent unreadable");
            }
            owner.Mark("percent-" + axis, pos);
            return Math.Max(0, Math.Min(100, 50 + pos));
        }

        public double HorizontalScrollPercent { get { return Percent("h", owner.HPos); } }
        public double VerticalScrollPercent { get { return Percent("v", owner.VPos); } }
        public double HorizontalViewSize { get { return 10; } }
        public double VerticalViewSize { get { return 10; } }
        public bool HorizontallyScrollable { get { return true; } }
        public bool VerticallyScrollable { get { return !owner.VMinusOne; } }
    }

    public class Fixture
    {
        readonly string folder;
        readonly string name;
        readonly Rectangle home;
        readonly Form form;
        readonly Timer timer;
        readonly Dictionary<string, Control> controls = new Dictionary<string, Control>();
        readonly Dictionary<IntPtr, string> names = new Dictionary<IntPtr, string>();
        readonly Dictionary<string, int> clicks = new Dictionary<string, int>();
        readonly List<string> removed = new List<string>();
        readonly List<string> events = new List<string>();
        int ack = 0;
        string cmdError = "";
        int activations = 0;
        int deactivations = 0;
        bool armLose = false;
        long loseRemovedAt = -1;
        int writeErrors = 0;
        int dropped = 0;
        IntPtr awayTo = IntPtr.Zero;
        Form spare;
        long spareHwnd = 0;
        IntPtr formHwnd = IntPtr.Zero;

        public static void Run(string folder, string name, int left, int top, int width, int height, int red, int green, int blue)
        {
            Application.SetUnhandledExceptionMode(UnhandledExceptionMode.CatchException);
            Application.ThreadException += delegate(object sender, System.Threading.ThreadExceptionEventArgs e)
            {
                try { File.AppendAllText(Path.Combine(folder, name + "-errors.log"), e.Exception.ToString() + Environment.NewLine); }
                catch (Exception) { }
            };
            Fixture fixture = new Fixture(folder, name, new Rectangle(left, top, width, height), Color.FromArgb(red, green, blue));
            Recorder recorder = new Recorder();
            recorder.Owner = fixture;
            Application.AddMessageFilter(recorder);
            Application.Run(fixture.form);
        }

        Fixture(string folder, string name, Rectangle home, Color colour)
        {
            this.folder = folder;
            this.name = name;
            this.home = home;
            form = new Form();
            form.Text = name;
            form.StartPosition = FormStartPosition.Manual;
            form.Bounds = home;
            form.BackColor = colour;
            form.Activated += delegate
            {
                activations++;
                Note("form", "activated", 0);
                if (armLose && controls.ContainsKey("Lose edit"))
                {
                    armLose = false;
                    Remove("Lose edit");
                    loseRemovedAt = Elapsed();
                }
            };
            form.Deactivate += delegate { deactivations++; Note("form", "deactivated", 0); };
            form.Shown += delegate
            {
                Rebuild();
                timer.Start();
            };
            timer = new Timer();
            timer.Interval = 30;
            timer.Tick += delegate
            {
                PollCommand();
                WriteState();
            };
        }

        // Milliseconds on the machine-wide performance counter, the clock the proof
        // and the other fixture use too, so their timelines line up.
        public static long Now()
        {
            return System.Diagnostics.Stopwatch.GetTimestamp() / (System.Diagnostics.Stopwatch.Frequency / 1000);
        }

        long Elapsed()
        {
            return Now();
        }

        public void Record(IntPtr hwnd, string kind, long value)
        {
            string target;
            lock (names)
            {
                if (!names.TryGetValue(hwnd, out target))
                {
                    target = "hwnd:" + hwnd.ToInt64().ToString("x");
                }
            }
            Note(target, kind, value);
        }

        // Also called from UI Automation provider threads.
        public void Note(string target, string kind, long value)
        {
            lock (events)
            {
                if (events.Count >= 3000)
                {
                    dropped++;
                    return;
                }
                events.Add("[" + Elapsed() + "," + J(target) + "," + J(kind) + "," + value + "]");
            }
        }

        T Add<T>(T control, string label, int x, int y, int w, int h) where T : Control
        {
            control.AccessibleName = label;
            control.Name = label.Replace(" ", "_");
            control.SetBounds(x, y, w, h);
            form.Controls.Add(control);
            controls[label] = control;
            lock (names) names[control.Handle] = label;
            string focusKey = label;
            control.GotFocus += delegate { Note(focusKey, "gotfocus", 0); };
            Button button = control as Button;
            if (button != null)
            {
                clicks[label] = 0;
                string key = label;
                button.Click += delegate { clicks[key] = clicks[key] + 1; };
            }
            return control;
        }

        FocusBox Box(string label, int x, int y, string mode, Control sink)
        {
            FocusBox box = new FocusBox();
            box.Text = "hello world";
            box.Mode = mode;
            box.Sink = sink;
            box.AwayTo = awayTo;
            Add(box, label, x, y, 230, 24);
            return box;
        }

        ListBox List(string label, string prefix, int x, int y, int w, int h)
        {
            ListBox list = new ListBox();
            list.IntegralHeight = false;
            for (int i = 1; i <= 200; i++) list.Items.Add(prefix + " " + i);
            Add(list, label, x, y, w, h);
            return list;
        }

        void Rebuild()
        {
            Control[] old = new Control[form.Controls.Count];
            form.Controls.CopyTo(old, 0);
            form.Controls.Clear();
            foreach (Control control in old) control.Dispose();
            controls.Clear();
            lock (names) names.Clear();
            clicks.Clear();
            removed.Clear();
            lock (events) events.Clear();
            dropped = 0;
            armLose = false;
            loseRemovedAt = -1;
            activations = 0;
            deactivations = 0;
            if (spare != null)
            {
                spare.Close();
                spare.Dispose();
                spare = null;
            }
            spareHwnd = 0;
            if (form.WindowState != FormWindowState.Normal) form.WindowState = FormWindowState.Normal;
            form.Bounds = home;
            lock (names) names[form.Handle] = "form";
            formHwnd = form.Handle;

            Button sink = Add(new Button(), "Focus sink", 260, 330, 120, 28);
            sink.Text = "Focus sink";
            Box("Filled edit", 10, 10, "normal", sink);
            Box("Click focus edit", 10, 42, "clickonly", sink);
            Box("Bounce edit", 10, 74, "bounce", sink);
            Box("Click away edit", 10, 106, "clickaway", sink);
            Box("Lose edit", 10, 138, "bounce", sink);
            Box("Victim edit", 10, 170, "normal", sink);
            Button victim = Add(new Button(), "Victim button", 10, 202, 140, 32);
            victim.Text = "Victim button";
            List("Victim list", "Row", 10, 242, 230, 90);
            Button pad = Add(new Button(), "Wheel pad", 260, 10, 230, 50);
            pad.Text = "Wheel pad";
            Button pad2 = Add(new Button(), "Wheel pad two", 260, 70, 230, 50);
            pad2.Text = "Wheel pad two";
            CustomScroller scroller = new CustomScroller();
            Add(scroller, "Custom scroller", 260, 130, 230, 60);
            scroller.OnGone = delegate { if (controls.ContainsKey("Custom scroller")) Remove("Custom scroller"); };
            scroller.UiThread = System.Threading.Thread.CurrentThread.ManagedThreadId;
            scroller.Steal = StealForeground;
            scroller.OnCall = delegate(string kind, long call) { Note("Custom scroller", kind, call); };
            List("Fixture list", "Item", 260, 200, 230, 120);

            foreach (KeyValuePair<string, Control> pair in controls)
            {
                TextBox text = pair.Value as TextBox;
                if (text != null) text.Select(5, 0);
            }
            form.ActiveControl = sink;
        }

        // Runs on a UI Automation provider thread, inside Scroll.
        long StealForeground(bool closeSpare)
        {
            if (closeSpare)
            {
                form.Invoke(new MethodInvoker(delegate
                {
                    if (spare != null)
                    {
                        spare.Close();
                        spare.Dispose();
                        spare = null;
                    }
                }));
            }
            IntPtr hwnd = formHwnd;
            if (Native.GetForegroundWindow() == hwnd) return 4;
            if (Native.SetForegroundWindow(hwnd) && Native.GetForegroundWindow() == hwnd) return 1;
            Native.keybd_event(0x12, 0, 0, UIntPtr.Zero);
            Native.keybd_event(0x12, 0, 2, UIntPtr.Zero);
            if (Native.SetForegroundWindow(hwnd) && Native.GetForegroundWindow() == hwnd) return 2;
            uint pid;
            uint other = Native.GetWindowThreadProcessId(Native.GetForegroundWindow(), out pid);
            uint mine = Native.GetCurrentThreadId();
            bool attached = other != 0 && Native.AttachThreadInput(mine, other, true);
            Native.SetForegroundWindow(hwnd);
            Native.BringWindowToTop(hwnd);
            if (attached) Native.AttachThreadInput(mine, other, false);
            return Native.GetForegroundWindow() == hwnd ? 3 : 0;
        }

        void Remove(string label)
        {
            Control control;
            if (!controls.TryGetValue(label, out control)) throw new ArgumentException("no control " + label);
            form.Controls.Remove(control);
            control.Dispose();
            controls.Remove(label);
            removed.Add(label);
        }

        void Replace(string label)
        {
            Control control;
            if (!controls.TryGetValue(label, out control)) throw new ArgumentException("no control " + label);
            Rectangle bounds = control.Bounds;
            Control replacement;
            if (control is TextBox)
            {
                TextBox text = new TextBox();
                text.Text = "replacement text";
                replacement = text;
            }
            else if (control is ListBox)
            {
                ListBox list = new ListBox();
                list.IntegralHeight = false;
                for (int i = 1; i <= 200; i++) list.Items.Add("Other " + i);
                replacement = list;
            }
            else
            {
                Button button = new Button();
                button.Text = "Replacement";
                replacement = button;
            }
            Remove(label);
            Add(replacement, "Replacement", bounds.X, bounds.Y, bounds.Width, bounds.Height);
            TextBox box = replacement as TextBox;
            if (box != null) box.Select(5, 0);
        }

        void PollCommand()
        {
            string path = Path.Combine(folder, name + "-cmd.txt");
            string line;
            try
            {
                if (!File.Exists(path)) return;
                line = File.ReadAllText(path).Trim();
            }
            catch (Exception)
            {
                return;
            }
            string[] parts = line.Split(' ');
            int seq;
            if (parts.Length < 2 || !int.TryParse(parts[0], out seq) || seq <= ack) return;
            cmdError = "";
            try
            {
                Execute(parts);
            }
            catch (Exception error)
            {
                cmdError = error.GetType().Name + ": " + error.Message;
            }
            ack = seq;
        }

        string Rest(string[] parts, int from)
        {
            return string.Join(" ", parts, from, parts.Length - from);
        }

        void Execute(string[] parts)
        {
            string op = parts[1];
            if (op == "reset") { Rebuild(); return; }
            if (op == "remove") { Remove(Rest(parts, 2)); return; }
            if (op == "replace") { Replace(Rest(parts, 2)); return; }
            if (op == "move")
            {
                form.Location = new Point(form.Left + int.Parse(parts[2]), form.Top + int.Parse(parts[3]));
                return;
            }
            if (op == "bounds")
            {
                form.Bounds = new Rectangle(int.Parse(parts[2]), int.Parse(parts[3]), int.Parse(parts[4]), int.Parse(parts[5]));
                return;
            }
            if (op == "arm-lose") { armLose = true; return; }
            if (op == "away")
            {
                awayTo = new IntPtr(long.Parse(parts[2]));
                foreach (Control control in controls.Values)
                {
                    FocusBox box = control as FocusBox;
                    if (box != null) box.AwayTo = awayTo;
                }
                return;
            }
            if (op == "scroller")
            {
                CustomScroller scroller = (CustomScroller)controls["Custom scroller"];
                lock (scroller)
                {
                    scroller.Mode = parts[2];
                    scroller.Calls = 0;
                }
                return;
            }
            if (op == "focus")
            {
                form.ActiveControl = controls[Rest(parts, 2)];
                return;
            }
            if (op == "minimize") { form.WindowState = FormWindowState.Minimized; return; }
            if (op == "spare-open")
            {
                // A second top-level window of this process, which a row can close.
                spare = new Form();
                spare.Text = name + " spare";
                spare.StartPosition = FormStartPosition.Manual;
                spare.Bounds = new Rectangle(home.X + 40, home.Y + 40, 300, 160);
                TextBox box = new TextBox();
                box.AccessibleName = "Spare edit";
                box.Text = "spare text";
                box.SetBounds(10, 10, 200, 24);
                spare.Controls.Add(box);
                spare.Show();
                spareHwnd = spare.Handle.ToInt64();
                lock (names)
                {
                    names[spare.Handle] = "spare form";
                    names[box.Handle] = "Spare edit";
                }
                return;
            }
            if (op == "spare-close")
            {
                spare.Close();
                spare.Dispose();
                spare = null;
                return;
            }
            if (op == "listtop")
            {
                ((ListBox)controls[Rest(parts, 3)]).TopIndex = int.Parse(parts[2]);
                return;
            }
            throw new ArgumentException("unknown op " + op);
        }

        static string J(string value)
        {
            if (value == null) return "null";
            StringBuilder sb = new StringBuilder("\"");
            foreach (char c in value)
            {
                if (c == '"' || c == '\\') sb.Append('\\').Append(c);
                else if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4"));
                else sb.Append(c);
            }
            return sb.Append('"').ToString();
        }

        static string B(bool value)
        {
            return value ? "true" : "false";
        }

        string ControlState(string label, Control control)
        {
            Rectangle screen = control.Parent != null ? control.Parent.RectangleToScreen(control.Bounds) : control.Bounds;
            StringBuilder sb = new StringBuilder();
            sb.Append("{\"hwnd\":").Append(control.Handle.ToInt64());
            sb.Append(",\"screen\":[").Append(screen.Left).Append(',').Append(screen.Top).Append(',').Append(screen.Right).Append(',').Append(screen.Bottom).Append(']');
            TextBox text = control as TextBox;
            if (text != null)
            {
                sb.Append(",\"text\":").Append(J(text.Text));
                sb.Append(",\"caret\":").Append(text.SelectionStart);
                sb.Append(",\"selection\":").Append(text.SelectionLength);
            }
            FocusBox box = control as FocusBox;
            if (box != null)
            {
                sb.Append(",\"mode\":").Append(J(box.Mode)).Append(",\"bounces\":").Append(box.Bounces);
            }
            ListBox list = control as ListBox;
            if (list != null)
            {
                sb.Append(",\"top\":").Append(list.TopIndex);
            }
            int count;
            if (clicks.TryGetValue(label, out count))
            {
                sb.Append(",\"clicks\":").Append(count);
            }
            CustomScroller scroller = control as CustomScroller;
            if (scroller != null)
            {
                lock (scroller)
                {
                    sb.Append(",\"mode\":").Append(J(scroller.Mode));
                    sb.Append(",\"calls\":").Append(scroller.Calls);
                    sb.Append(",\"vpos\":").Append(scroller.VPos);
                    sb.Append(",\"hpos\":").Append(scroller.HPos);
                    sb.Append(",\"last_error\":").Append(J(scroller.LastError));
                }
            }
            return sb.Append('}').ToString();
        }

        void WriteState()
        {
            try
            {
                string focus;
                IntPtr focused = Native.GetFocus();
                if (focused == IntPtr.Zero) focus = "";
                else if (!names.TryGetValue(focused, out focus)) focus = "hwnd:" + focused.ToInt64().ToString("x");
                StringBuilder sb = new StringBuilder();
                sb.Append("{\"pid\":").Append(System.Diagnostics.Process.GetCurrentProcess().Id);
                sb.Append(",\"hwnd\":").Append(form.Handle.ToInt64());
                sb.Append(",\"ack\":").Append(ack);
                sb.Append(",\"cmd_error\":").Append(J(cmdError));
                sb.Append(",\"elapsed_ms\":").Append(Elapsed());
                sb.Append(",\"activations\":").Append(activations);
                sb.Append(",\"deactivations\":").Append(deactivations);
                sb.Append(",\"active\":").Append(B(Form.ActiveForm == form));
                sb.Append(",\"focus\":").Append(J(focus));
                sb.Append(",\"lose_armed\":").Append(B(armLose));
                sb.Append(",\"spare_hwnd\":").Append(spareHwnd);
                sb.Append(",\"spare_open\":").Append(B(spare != null));
                sb.Append(",\"window_state\":").Append(J(form.WindowState.ToString()));
                sb.Append(",\"lose_removed_at\":").Append(loseRemovedAt);
                sb.Append(",\"bounds\":[").Append(form.Left).Append(',').Append(form.Top).Append(',').Append(form.Width).Append(',').Append(form.Height).Append(']');
                sb.Append(",\"write_errors\":").Append(writeErrors);
                sb.Append(",\"dropped_events\":").Append(dropped);
                sb.Append(",\"removed\":[");
                for (int i = 0; i < removed.Count; i++)
                {
                    if (i > 0) sb.Append(',');
                    sb.Append(J(removed[i]));
                }
                sb.Append("],\"controls\":{");
                bool first = true;
                foreach (KeyValuePair<string, Control> pair in controls)
                {
                    if (!first) sb.Append(',');
                    first = false;
                    sb.Append(J(pair.Key)).Append(':').Append(ControlState(pair.Key, pair.Value));
                }
                string joined;
                lock (events) joined = string.Join(",", events.ToArray());
                sb.Append("},\"events\":[").Append(joined).Append("]}");
                string temp = Path.Combine(folder, name + ".json.tmp");
                File.WriteAllText(temp, sb.ToString());
                // MOVEFILE_REPLACE_EXISTING; fails harmlessly while a reader holds the file.
                if (!Native.MoveFileEx(temp, Path.Combine(folder, name + ".json"), 1)) writeErrors++;
            }
            catch (Exception)
            {
                writeErrors++;
            }
        }
    }
}
