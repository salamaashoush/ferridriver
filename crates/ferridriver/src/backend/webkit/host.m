// WebKit host subprocess — standalone Objective-C implementation.
// Architecture derived from studying Bun's webview implementation (MIT License).
//
// Single-threaded subprocess that runs WKWebView on the main thread.
// Uses CFRunLoop + CFFileDescriptor for non-blocking IPC (no polling).
// Binary frame protocol over Unix socketpair.
//
// Copyright (c) Oven-sh (Bun) - original architecture
// Adapted for ferridriver

#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>
#import <CoreFoundation/CoreFoundation.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#import <objc/runtime.h>
#import <objc/message.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <string.h>
#include <stdlib.h>
#include <sys/mman.h>

// ─── Frame protocol (matches Rust side) ─────────────────────────────────────

#pragma pack(push, 1)
typedef struct {
    uint32_t len;       // payload length
    uint32_t req_id;    // correlation ID (echoed in response)
    uint8_t  op;        // operation code
} Frame;
#pragma pack(pop)

enum Op {
    OP_CREATE_VIEW = 1,
    OP_NAVIGATE = 2,
    OP_EVALUATE = 3,
    OP_SCREENSHOT = 4,
    OP_CLOSE = 5,
    OP_RELOAD = 9,
    OP_CLICK = 10,
    OP_TYPE = 11,
    OP_PRESS_KEY = 12,
    OP_KEY_DOWN = 13,
    OP_KEY_UP = 14,
    OP_GET_URL = 20,
    OP_GET_TITLE = 21,
    OP_LIST_VIEWS = 22,
    OP_SET_USER_AGENT = 30,
    OP_WAIT_NAV = 40,
    OP_SET_FILE_INPUT = 50,
    OP_SET_VIEWPORT = 51,
    OP_GET_COOKIES = 60,
    OP_SET_COOKIE = 61,
    OP_DELETE_COOKIE = 62,
    OP_CLEAR_COOKIES = 63,
    OP_LOAD_HTML = 64,
    OP_ADD_INIT_SCRIPT = 65,
    OP_MOUSE_EVENT = 66,
    OP_SET_LOCALE = 67,
    OP_SET_TIMEZONE = 68,
    OP_EMULATE_MEDIA = 69,
    OP_ACCESSIBILITY_TREE = 70,
    OP_ROUTE_REQUEST = 71,
    OP_SHUTDOWN = 255,
};

enum Rep {
    REP_OK = 1,
    REP_ERROR = 2,
    REP_VALUE = 3,
    REP_VIEW_CREATED = 4,
    REP_VIEW_LIST = 5,
    REP_BINARY = 6,
    REP_SHM_SCREENSHOT = 7,
    REP_CONSOLE_EVENT = 8,  // unsolicited: payload = str level + str text
    REP_DIALOG_EVENT = 9,   // unsolicited: payload = str type + str message + str action
    REP_NET_EVENT = 10,     // unsolicited: payload = str id + str method + str url + str resourceType
};

// ─── JS-based accessibility tree builder ────────────────────────────────────
// Used when the native NSAccessibility tree is empty (e.g. data: URLs where
// the WebContent process doesn't establish the AX bridge).  Walks the DOM
// and maps HTML elements / ARIA attributes to the same JSON format as the
// native walker, so the Rust side parses it identically.
static NSString *g_ax_tree_js = @"(function(){"
    "var nodes=[],seq=0;"
    "var RM={'A':'link','BUTTON':'button','INPUT':'textbox','TEXTAREA':'textbox',"
    "'SELECT':'combobox','IMG':'img','H1':'heading','H2':'heading','H3':'heading',"
    "'H4':'heading','H5':'heading','H6':'heading','NAV':'navigation','MAIN':'main',"
    "'HEADER':'banner','FOOTER':'contentinfo','ASIDE':'complementary','FORM':'form',"
    "'TABLE':'table','TR':'row','TD':'cell','TH':'columnheader','UL':'list','OL':'list',"
    "'LI':'listitem','LABEL':'label','PROGRESS':'progressbar','DIALOG':'dialog',"
    "'DETAILS':'group','SECTION':'generic','ARTICLE':'article','SUMMARY':'button'};"
    "var HL={'H1':1,'H2':2,'H3':3,'H4':4,'H5':5,'H6':6};"
    "nodes.push({nodeId:'n'+(seq++),role:'RootWebArea',name:document.title||'',properties:[],ignored:false});"
    "function walk(el,pid){"
    "if(!el||el.nodeType!==1)return;"
    "var tag=el.tagName,ar=el.getAttribute('role'),role=ar||RM[tag]||'';"
    "var nm=el.getAttribute('aria-label')||el.getAttribute('alt')||'';"
    "if(!nm&&(tag==='BUTTON'||tag==='A'||tag==='LABEL'))nm=el.textContent.trim().substring(0,200);"
    "var isLeafText=!role&&el.children.length===0&&el.textContent.trim().length>0;"
    "if(role||isLeafText){"
    "var nid='n'+(seq++),node={nodeId:nid,parentId:pid,role:role||(isLeafText?'StaticText':'generic'),"
    "properties:[],ignored:false};"
    "if(nm)node.name=nm;"
    "if(isLeafText)node.name=el.textContent.trim().substring(0,500);"
    "var hl=HL[tag];if(hl)node.properties.push({name:'level',value:hl});"
    "if(tag==='INPUT'||tag==='TEXTAREA'){"
    "var t=el.type||'text';"
    "if(t==='checkbox')node.role='checkbox';"
    "else if(t==='radio')node.role='radio';"
    "else if(t==='submit'||t==='button')node.role='button';"
    "if(el.value)node.properties.push({name:'value',value:el.value});"
    "if(el.disabled)node.properties.push({name:'disabled',value:true});"
    "if(el.required)node.properties.push({name:'required',value:true});"
    "}"
    "if(el.getAttribute('aria-checked'))node.properties.push({name:'checked',value:el.getAttribute('aria-checked')==='true'});"
    "if(el.getAttribute('aria-expanded'))node.properties.push({name:'expanded',value:el.getAttribute('aria-expanded')==='true'});"
    "if(el.getAttribute('aria-selected'))node.properties.push({name:'selected',value:el.getAttribute('aria-selected')==='true'});"
    "nodes.push(node);"
    "for(var i=0;i<el.children.length;i++)walk(el.children[i],nid);"
    "}else{"
    "for(var i=0;i<el.children.length;i++)walk(el.children[i],pid);"
    "}"
    "}"
    "if(document.body)walk(document.body,'n0');"
    "return JSON.stringify(nodes);"
    "})()";

// ─── FrameWriter (port of Bun's FrameWriter) ────────────────────────────────
// Uses writev for initial write. On partial write, queues remainder and
// enables kCFFileDescriptorWriteCallBack to drain. No spinning, no blocking.

#include <sys/uio.h>

static int g_fd = -1;
// TODO: headful mode needs proper NSApp event loop integration
// static BOOL g_headful = NO;
static CFFileDescriptorRef g_cffd = NULL;
static NSMutableData *g_write_queue = nil;

static void writer_on_writable(void) {
    while (g_write_queue.length > 0) {
        ssize_t w = write(g_fd, g_write_queue.bytes, g_write_queue.length);
        if (w < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                CFFileDescriptorEnableCallBacks(g_cffd, kCFFileDescriptorWriteCallBack);
            }
            return;
        }
        [g_write_queue replaceBytesInRange:NSMakeRange(0, w) withBytes:NULL length:0];
    }
}

static void writer_queue_from(const uint8_t *a, size_t alen,
                               const uint8_t *b, size_t blen,
                               size_t skip) {
    if (skip < alen) {
        [g_write_queue appendBytes:a + skip length:alen - skip];
        skip = 0;
    } else {
        skip -= alen;
    }
    if (blen > skip) {
        [g_write_queue appendBytes:b + skip length:blen - skip];
    }
}

static void write_frame(uint32_t req_id, uint8_t rep, const void *payload, uint32_t len) {
    Frame h = { len, req_id, rep };

    if (g_write_queue.length == 0) {
        // Try writev first — fast path when socket buffer has space
        struct iovec iov[2] = {
            { &h, sizeof(h) },
            { (void*)payload, len },
        };
        int iovcnt = (payload && len > 0) ? 2 : 1;
        ssize_t w = writev(g_fd, iov, iovcnt);
        size_t total = sizeof(h) + len;
        if (w == (ssize_t)total) return; // all written
        if (w < 0) {
            if (errno != EAGAIN && errno != EWOULDBLOCK) return; // peer gone
            w = 0;
        }
        writer_queue_from((const uint8_t*)&h, sizeof(h),
                          (const uint8_t*)payload, len, (size_t)w);
    } else {
        writer_queue_from((const uint8_t*)&h, sizeof(h),
                          (const uint8_t*)payload, len, 0);
    }
    CFFileDescriptorEnableCallBacks(g_cffd, kCFFileDescriptorWriteCallBack);
}

static void write_frame_str(uint32_t req_id, uint8_t rep, NSString *s) {
    const char *utf8 = [s UTF8String];
    uint32_t slen = (uint32_t)strlen(utf8);
    uint32_t total = 4 + slen;
    uint8_t *buf = malloc(total);
    memcpy(buf, &slen, 4);
    memcpy(buf + 4, utf8, slen);
    write_frame(req_id, rep, buf, total);
    free(buf);
}

static NSString *read_str(const uint8_t *data, uint32_t data_len, uint32_t *offset) {
    if (*offset + 4 > data_len) return @"";
    uint32_t slen;
    memcpy(&slen, data + *offset, 4);
    *offset += 4;
    if (*offset + slen > data_len) return @"";
    NSString *s = [[NSString alloc] initWithBytes:data + *offset
                                           length:slen
                                         encoding:NSUTF8StringEncoding];
    *offset += slen;
    return s ?: @"";
}

static uint64_t read_u64(const uint8_t *data, uint32_t data_len, uint32_t *offset) {
    if (*offset + 8 > data_len) return 0;
    uint64_t v;
    memcpy(&v, data + *offset, 8);
    *offset += 8;
    return v;
}

// ─── Navigation delegate ────────────────────────────────────────────────────

@interface FDNavDelegate : NSObject <WKNavigationDelegate, WKScriptMessageHandler, WKScriptMessageHandlerWithReply>
@property (nonatomic, strong) NSMutableDictionary<NSNumber*, void(^)(NSError*)> *waiters;
@end

@implementation FDNavDelegate
- (instancetype)init {
    self = [super init];
    _waiters = [NSMutableDictionary new];
    return self;
}

// WKScriptMessageHandler — fires instantly via CFRunLoop for console, dialog, network events.
- (void)userContentController:(WKUserContentController *)ctrl
      didReceiveScriptMessage:(WKScriptMessage *)message {
    NSDictionary *body = message.body;
    if (![body isKindOfClass:[NSDictionary class]]) return;

    if ([message.name isEqualToString:@"fdConsole"]) {
        NSString *level = body[@"level"] ?: @"log";
        NSString *text = body[@"text"] ?: @"";
        const char *levelUtf8 = [level UTF8String];
        const char *textUtf8 = [text UTF8String];
        uint32_t levelLen = (uint32_t)strlen(levelUtf8);
        uint32_t textLen = (uint32_t)strlen(textUtf8);
        uint32_t total = 4 + levelLen + 4 + textLen;
        uint8_t *buf = malloc(total);
        memcpy(buf, &levelLen, 4);
        memcpy(buf + 4, levelUtf8, levelLen);
        memcpy(buf + 4 + levelLen, &textLen, 4);
        memcpy(buf + 4 + levelLen + 4, textUtf8, textLen);
        write_frame(0, REP_CONSOLE_EVENT, buf, total);
        free(buf);
    }
    else if ([message.name isEqualToString:@"fdDialog"]) {
        NSString *type = body[@"type"] ?: @"alert";
        NSString *msg = body[@"message"] ?: @"";
        NSString *action = body[@"action"] ?: @"accepted";
        const char *t = [type UTF8String], *m = [msg UTF8String], *a = [action UTF8String];
        uint32_t tl = (uint32_t)strlen(t), ml = (uint32_t)strlen(m), al = (uint32_t)strlen(a);
        uint32_t total = 12 + tl + ml + al;
        uint8_t *buf = malloc(total);
        uint32_t off = 0;
        memcpy(buf+off, &tl, 4); off+=4; memcpy(buf+off, t, tl); off+=tl;
        memcpy(buf+off, &ml, 4); off+=4; memcpy(buf+off, m, ml); off+=ml;
        memcpy(buf+off, &al, 4); off+=4; memcpy(buf+off, a, al); off+=al;
        write_frame(0, REP_DIALOG_EVENT, buf, total);
        free(buf);
    }
    else if ([message.name isEqualToString:@"fdNetwork"]) {
        NSString *rid = body[@"id"] ?: @"";
        NSString *method = body[@"method"] ?: @"GET";
        NSString *url = body[@"url"] ?: @"";
        NSString *resType = body[@"resourceType"] ?: @"Fetch";
        const char *r = [rid UTF8String], *m = [method UTF8String], *u = [url UTF8String], *rt = [resType UTF8String];
        uint32_t rl = (uint32_t)strlen(r), ml = (uint32_t)strlen(m), ul = (uint32_t)strlen(u), rtl = (uint32_t)strlen(rt);
        uint32_t total = 16 + rl + ml + ul + rtl;
        uint8_t *buf = malloc(total);
        uint32_t off = 0;
        memcpy(buf+off, &rl, 4); off+=4; memcpy(buf+off, r, rl); off+=rl;
        memcpy(buf+off, &ml, 4); off+=4; memcpy(buf+off, m, ml); off+=ml;
        memcpy(buf+off, &ul, 4); off+=4; memcpy(buf+off, u, ul); off+=ul;
        memcpy(buf+off, &rtl, 4); off+=4; memcpy(buf+off, rt, rtl); off+=rtl;
        write_frame(0, REP_NET_EVENT, buf, total);
        free(buf);
    }
}
// Global dictionary for pending route reply handlers, keyed by req_id.
// Populated by WKScriptMessageHandlerWithReply, resolved by dispatch_frame.
static NSMutableDictionary<NSNumber*, void(^)(id, NSString*)> *g_pending_routes = nil;

// WKScriptMessageHandlerWithReply -- used for route interception.
// JS calls postMessage() and awaits the reply. We send an IPC frame to Rust,
// Rust runs the route handler, sends back a response frame. dispatch_frame
// picks it up and calls the stored replyHandler, which resolves the JS Promise.
- (void)userContentController:(WKUserContentController *)ctrl
      didReceiveScriptMessage:(WKScriptMessage *)message
                 replyHandler:(void (^)(id _Nullable, NSString * _Nullable))replyHandler {
    if (![message.name isEqualToString:@"fdRoute"]) {
        replyHandler(nil, @"Unknown handler");
        return;
    }
    NSDictionary *body = message.body;
    if (![body isKindOfClass:[NSDictionary class]]) {
        replyHandler(@{@"action": @"continue"}, nil);
        return;
    }

    NSString *url = body[@"url"] ?: @"";
    NSString *method = body[@"method"] ?: @"GET";
    NSString *headersJson = body[@"headers"] ?: @"{}";
    NSString *postData = body[@"postData"] ?: @"";

    const char *u = [url UTF8String], *m = [method UTF8String], *h = [headersJson UTF8String], *p = [postData UTF8String];
    uint32_t ul = (uint32_t)strlen(u), ml = (uint32_t)strlen(m), hl = (uint32_t)strlen(h), pl = (uint32_t)strlen(p);
    uint32_t total = 16 + ul + ml + hl + pl;
    uint8_t *buf = malloc(total);
    uint32_t off = 0;
    memcpy(buf+off, &ul, 4); off+=4; memcpy(buf+off, u, ul); off+=ul;
    memcpy(buf+off, &ml, 4); off+=4; memcpy(buf+off, m, ml); off+=ml;
    memcpy(buf+off, &hl, 4); off+=4; memcpy(buf+off, h, hl); off+=hl;
    memcpy(buf+off, &pl, 4); off+=4; memcpy(buf+off, p, pl); off+=pl;

    static uint32_t route_seq = 50000;
    uint32_t rid = route_seq++;

    // Store the reply handler. It will be called from dispatch_frame when
    // the Rust parent sends back REP_VALUE with this req_id.
    if (!g_pending_routes) g_pending_routes = [NSMutableDictionary new];
    g_pending_routes[@(rid)] = [replyHandler copy];

    // Send as rep=11 (REP_ROUTE_REQUEST) so the Rust reader recognizes it
    write_frame(rid, 11, buf, total);
    free(buf);
    // replyHandler will be called later from dispatch_frame
}

- (void)webView:(WKWebView *)wv didFinishNavigation:(WKNavigation *)nav {
    NSNumber *key = @((uintptr_t)wv);
    void(^block)(NSError*) = _waiters[key];
    if (block) { [_waiters removeObjectForKey:key]; block(nil); }
}
- (void)webView:(WKWebView *)wv didFailNavigation:(WKNavigation *)nav withError:(NSError *)error {
    NSNumber *key = @((uintptr_t)wv);
    void(^block)(NSError*) = _waiters[key];
    if (block) { [_waiters removeObjectForKey:key]; block(error); }
}
- (void)webView:(WKWebView *)wv didFailProvisionalNavigation:(WKNavigation *)nav withError:(NSError *)error {
    NSNumber *key = @((uintptr_t)wv);
    void(^block)(NSError*) = _waiters[key];
    if (block) { [_waiters removeObjectForKey:key]; block(error); }
}
// Emit REP_NET_EVENT for main-frame navigations so they appear in network diagnostics.
// The JS interceptors only capture fetch/XHR; this catches the document navigation itself.
- (void)webView:(WKWebView *)wv decidePolicyForNavigationAction:(WKNavigationAction *)act
    decisionHandler:(void (^)(WKNavigationActionPolicy))handler {
    if (act.targetFrame.isMainFrame && act.request.URL) {
        NSString *url = act.request.URL.absoluteString;
        NSString *method = act.request.HTTPMethod ?: @"GET";
        static int navSeq = 0;
        NSString *rid = [NSString stringWithFormat:@"nav%d", navSeq++];
        NSString *resType = @"Document";
        const char *r = [rid UTF8String], *m = [method UTF8String], *u = [url UTF8String], *rt = [resType UTF8String];
        uint32_t rl = (uint32_t)strlen(r), ml = (uint32_t)strlen(m), ul = (uint32_t)strlen(u), rtl = (uint32_t)strlen(rt);
        uint32_t total = 16 + rl + ml + ul + rtl;
        uint8_t *buf = malloc(total);
        uint32_t off = 0;
        memcpy(buf+off, &rl, 4); off+=4; memcpy(buf+off, r, rl); off+=rl;
        memcpy(buf+off, &ml, 4); off+=4; memcpy(buf+off, m, ml); off+=ml;
        memcpy(buf+off, &ul, 4); off+=4; memcpy(buf+off, u, ul); off+=ul;
        memcpy(buf+off, &rtl, 4); off+=4; memcpy(buf+off, rt, rtl); off+=rtl;
        write_frame(0, REP_NET_EVENT, buf, total);
        free(buf);
    }
    handler(WKNavigationActionPolicyAllow);
}
@end

// ─── Custom window (isVisible/isKeyWindow/screen overrides) ─────────────────
// Same pattern as Bun's BunHostWindow — runtime-registered subclass.
// isVisible=YES, isKeyWindow=YES, screen=mainScreen so WebKit thinks
// the window is visible and rendering pipeline ticks.

@interface FDHostWindow : NSWindow
@property (nonatomic) CGFloat emulatedScaleFactor;
@end

@implementation FDHostWindow
- (instancetype)initWithContentRect:(NSRect)rect styleMask:(NSWindowStyleMask)style backing:(NSBackingStoreType)buf defer:(BOOL)flag {
    self = [super initWithContentRect:rect styleMask:style backing:buf defer:flag];
    _emulatedScaleFactor = 1.0;
    [self setAcceptsMouseMovedEvents:YES];
    return self;
}
- (BOOL)isVisible { return YES; }
- (BOOL)isKeyWindow { return YES; }
- (NSScreen *)screen { return [[NSScreen screens] firstObject]; }
- (CGFloat)backingScaleFactor { return _emulatedScaleFactor; }
- (void)noResponderFor:(SEL)sel {} // suppress beep
@end

// ─── Host state ─────────────────────────────────────────────────────────────

typedef struct {
    WKWebView *webview;
    FDHostWindow *window;
} ViewEntry;

static NSMutableDictionary<NSNumber*, NSValue*> *g_views;
static uint64_t g_next_vid = 1;
static FDNavDelegate *g_nav_delegate;
static NSMutableData *g_rx;
static CFFileDescriptorRef g_cffd;

// ─── Command dispatch ───────────────────────────────────────────────────────

static void dispatch_frame(uint32_t req_id, uint8_t op,
                           const uint8_t *payload, uint32_t payload_len);

static void cf_callback(CFFileDescriptorRef cffd, CFOptionFlags flags, void *info) {
    (void)cffd;
    (void)info;
    if (flags & kCFFileDescriptorWriteCallBack) writer_on_writable();
    if (!(flags & kCFFileDescriptorReadCallBack)) return;
    // Read all available data (to EAGAIN) — same as Bun's Host::onReadable
    uint8_t tmp[8192];
    for (;;) {
        ssize_t n = read(g_fd, tmp, sizeof(tmp));
        if (n > 0) {
            [g_rx appendBytes:tmp length:n];
            continue;
        }
        if (n == 0) {
            // Parent died — exit cleanly
            [g_views removeAllObjects];
            CFRunLoopStop(CFRunLoopGetCurrent());
            return;
        }
        if (errno == EINTR) continue;
        break; // EAGAIN — drained
    }

    // Parse complete frames
    const uint8_t *base = g_rx.bytes;
    NSUInteger total = g_rx.length;
    NSUInteger off = 0;

    while (total - off >= sizeof(Frame)) {
        Frame h;
        memcpy(&h, base + off, sizeof(h));
        if (total - off < sizeof(Frame) + h.len) break; // partial
        dispatch_frame(h.req_id, h.op, base + off + sizeof(Frame), h.len);
        off += sizeof(Frame) + h.len;
    }

    if (off > 0) {
        [g_rx replaceBytesInRange:NSMakeRange(0, off) withBytes:NULL length:0];
    }

    // Re-enable read callback (CFFileDescriptor disarms after each fire)
    CFFileDescriptorEnableCallBacks(g_cffd, kCFFileDescriptorReadCallBack);
}

static ViewEntry *get_view(uint64_t vid) {
    NSValue *v = g_views[@(vid)];
    return v ? (ViewEntry*)[v pointerValue] : NULL;
}

static void dispatch_frame(uint32_t req_id, uint8_t op,
                           const uint8_t *payload, uint32_t payload_len) {
    @autoreleasepool {
    switch (op) {
        case OP_CREATE_VIEW: {
            uint32_t off = 0;
            NSString *url = read_str(payload, payload_len, &off);

            WKWebViewConfiguration *config = [[WKWebViewConfiguration alloc] init];
            [config setWebsiteDataStore:[WKWebsiteDataStore nonPersistentDataStore]];

            // Disable process suppression (private API, use performSelector)
            SEL suppressSel = NSSelectorFromString(@"_setPageVisibilityBasedProcessSuppressionEnabled:");
            if ([config respondsToSelector:suppressSel]) {
                ((void(*)(id,SEL,BOOL))objc_msgSend)(config, suppressSel, NO);
            }

            // ── Message handlers: console, dialog, network ──
            // All fire instantly via CFRunLoop → WKScriptMessageHandler → binary IPC frame.
            // No polling. Injected at document start so they survive navigation.

            [config.userContentController addScriptMessageHandler:g_nav_delegate name:@"fdConsole"];
            [config.userContentController addScriptMessageHandler:g_nav_delegate name:@"fdDialog"];
            [config.userContentController addScriptMessageHandler:g_nav_delegate name:@"fdNetwork"];
            // Route handler with reply support (macOS 11+)
            if (@available(macOS 11.0, *)) {
                [config.userContentController addScriptMessageHandler:g_nav_delegate contentWorld:WKContentWorld.pageWorld name:@"fdRoute"];
            }

            // Console capture
            NSString *consoleJS = @"(function(){if(window.__fd_con)return;window.__fd_con=1;"
                "var h=webkit.messageHandlers.fdConsole;"
                "['log','warn','error','info','debug','trace'].forEach(function(l){"
                "var o=console[l];console[l]=function(){"
                "try{h.postMessage({level:l,text:Array.prototype.map.call(arguments,"
                "function(a){try{return typeof a==='string'?a:JSON.stringify(a)}"
                "catch(e){return String(a)}}).join(' ')})}catch(e){}"
                "return o.apply(console,arguments)}})})()";
            [config.userContentController addUserScript:[[WKUserScript alloc]
                initWithSource:consoleJS
                injectionTime:WKUserScriptInjectionTimeAtDocumentStart
                forMainFrameOnly:NO]];

            // Dialog auto-dismiss (alert/confirm/prompt)
            NSString *dialogJS = @"(function(){if(window.__fd_dlg)return;window.__fd_dlg=1;"
                "var h=webkit.messageHandlers.fdDialog;"
                "window.alert=function(m){try{h.postMessage({type:'alert',message:String(m||''),action:'accepted'})}catch(e){}};"
                "window.confirm=function(m){try{h.postMessage({type:'confirm',message:String(m||''),action:'accepted'})}catch(e){}return true;};"
                "window.prompt=function(m){try{h.postMessage({type:'prompt',message:String(m||''),action:'dismissed'})}catch(e){}return null;};"
                "})()";
            [config.userContentController addUserScript:[[WKUserScript alloc]
                initWithSource:dialogJS
                injectionTime:WKUserScriptInjectionTimeAtDocumentStart
                forMainFrameOnly:NO]];

            // Network observation + route interception (fetch + XMLHttpRequest)
            // Routes are patterns stored in __fd_routes array. When a fetch/XHR URL
            // matches a pattern, we call fdRoute.postMessage() which round-trips to Rust
            // for the route action (fulfill/continue/abort).
            NSString *networkJS = @"(function(){if(window.__fd_net)return;window.__fd_net=1;"
                "var hNet=webkit.messageHandlers.fdNetwork;var seq=0;"
                "window.__fd_routes=window.__fd_routes||[];"
                "function matchRoute(url){"
                  "for(var i=0;i<window.__fd_routes.length;i++){"
                    "if(window.__fd_routes[i].test(url))return true;"
                  "}"
                  "return false;"
                "}"
                // fetch interceptor
                "var origFetch=window.fetch;"
                "window.fetch=function(input,opts){"
                  "var method=(opts&&opts.method)||'GET';"
                  "var u=typeof input==='string'?input:(input&&input.url||'');"
                  "try{hNet.postMessage({id:'f'+(seq++),method:method,url:u,resourceType:'Fetch'})}catch(e){}"
                  "if(!matchRoute(u))return origFetch.apply(this,arguments);"
                  // Route matches -- ask Rust for the action
                  "var hdrs='{}';"
                  "try{if(opts&&opts.headers){hdrs=JSON.stringify(Object.fromEntries("
                    "opts.headers instanceof Headers?opts.headers.entries():Object.entries(opts.headers)))}}catch(e){}"
                  "var body=(opts&&opts.body)||'';"
                  "return webkit.messageHandlers.fdRoute.postMessage("
                    "{url:u,method:method,headers:hdrs,postData:typeof body==='string'?body:''}"
                  ").then(function(action){"
                    "if(!action||action.action==='continue')return origFetch.apply(null,[input,opts]);"
                    "if(action.action==='abort')throw new TypeError('Request blocked by route');"
                    "if(action.action==='fulfill'){"
                      "var h=new Headers();"
                      "if(action.headers){for(var k in action.headers)h.set(k,action.headers[k])}"
                      "if(action.contentType)h.set('content-type',action.contentType);"
                      "return new Response(action.body||'',{status:action.status||200,headers:h})"
                    "}"
                    "return origFetch.apply(null,[input,opts]);"
                  "});"
                "};"
                // XHR interceptor
                "var origOpen=XMLHttpRequest.prototype.open;"
                "var origSend=XMLHttpRequest.prototype.send;"
                "XMLHttpRequest.prototype.open=function(method,url){"
                  "this.__fd_method=method;this.__fd_url=url;"
                  "try{hNet.postMessage({id:'x'+(seq++),method:method,url:url,resourceType:'XHR'})}catch(e){}"
                  "return origOpen.apply(this,arguments);"
                "};"
                "XMLHttpRequest.prototype.send=function(body){"
                  "var self=this,url=this.__fd_url||'',method=this.__fd_method||'GET';"
                  "if(!matchRoute(url))return origSend.apply(this,arguments);"
                  "webkit.messageHandlers.fdRoute.postMessage("
                    "{url:url,method:method,headers:'{}',postData:typeof body==='string'?body:''}"
                  ").then(function(action){"
                    "if(!action||action.action==='continue')return origSend.apply(self,[body]);"
                    "if(action.action==='abort'){"
                      "Object.defineProperty(self,'status',{get:function(){return 0}});"
                      "Object.defineProperty(self,'readyState',{get:function(){return 4}});"
                      "self.dispatchEvent(new Event('error'));"
                      "return;"
                    "}"
                    "if(action.action==='fulfill'){"
                      "Object.defineProperty(self,'status',{get:function(){return action.status||200}});"
                      "Object.defineProperty(self,'responseText',{get:function(){return action.body||''}});"
                      "Object.defineProperty(self,'response',{get:function(){return action.body||''}});"
                      "Object.defineProperty(self,'readyState',{get:function(){return 4}});"
                      "self.dispatchEvent(new Event('readystatechange'));"
                      "self.dispatchEvent(new Event('load'));"
                      "return;"
                    "}"
                    "origSend.apply(self,[body]);"
                  "});"
                "};"
                "})()";
            [config.userContentController addUserScript:[[WKUserScript alloc]
                initWithSource:networkJS
                injectionTime:WKUserScriptInjectionTimeAtDocumentStart
                forMainFrameOnly:NO]];

            WKWebView *wv = [[WKWebView alloc]
                initWithFrame:NSMakeRect(0, 0, 1280, 720)
                configuration:config];
            [wv setNavigationDelegate:g_nav_delegate];

            // Enable enhanced-UI accessibility so WKWebView creates its
            // web-content AX tree bridge (needed for data: URLs especially).
            // Use performSelector to avoid deprecated-API warning.
            SEL axSel = NSSelectorFromString(@"accessibilitySetOverrideValue:forAttribute:");
            if ([wv respondsToSelector:axSel]) {
                ((BOOL(*)(id,SEL,id,NSString*))objc_msgSend)(
                    wv, axSel, @YES, @"AXEnhancedUserInterface");
            }

            // Add tracking area so mouseMoved events propagate to the web content
            NSTrackingArea *trackingArea = [[NSTrackingArea alloc]
                initWithRect:wv.bounds
                options:(NSTrackingMouseMoved | NSTrackingActiveAlways | NSTrackingInVisibleRect)
                owner:wv
                userInfo:nil];
            [wv addTrackingArea:trackingArea];

            // Disable occlusion detection (private API)
            SEL occSel = NSSelectorFromString(@"_setWindowOcclusionDetectionEnabled:");
            if ([wv respondsToSelector:occSel]) {
                ((void(*)(id,SEL,BOOL))objc_msgSend)(wv, occSel, NO);
            }

            // Disable text substitution (these are NSTextView methods inherited by WKWebView)
            SEL quoteSel = NSSelectorFromString(@"setAutomaticQuoteSubstitutionEnabled:");
            SEL dashSel = NSSelectorFromString(@"setAutomaticDashSubstitutionEnabled:");
            SEL replaceSel = NSSelectorFromString(@"setAutomaticTextReplacementEnabled:");
            if ([wv respondsToSelector:quoteSel])
                ((void(*)(id,SEL,BOOL))objc_msgSend)(wv, quoteSel, NO);
            if ([wv respondsToSelector:dashSel])
                ((void(*)(id,SEL,BOOL))objc_msgSend)(wv, dashSel, NO);
            if ([wv respondsToSelector:replaceSel])
                ((void(*)(id,SEL,BOOL))objc_msgSend)(wv, replaceSel, NO);

            FDHostWindow *win = [[FDHostWindow alloc]
                initWithContentRect:NSMakeRect(0, 0, 1280, 720)
                styleMask:NSWindowStyleMaskBorderless
                backing:NSBackingStoreBuffered
                defer:NO];
            [win setReleasedWhenClosed:NO];
            [win setAlphaValue:0.0];
            [win setIgnoresMouseEvents:YES];
            [win setContentView:wv];
            [win makeKeyAndOrderFront:nil];

            if (url.length > 0 && ![url isEqualToString:@"about:blank"]) {
                NSURL *nsurl = [NSURL URLWithString:url];
                if (nsurl) [wv loadRequest:[NSURLRequest requestWithURL:nsurl]];
            } else {
                // about:blank skips navigation so WKUserScripts don't fire.
                // Evaluate them manually so overrides (console, dialog, network)
                // are active from the start.
                for (WKUserScript *s in config.userContentController.userScripts) {
                    [wv evaluateJavaScript:s.source completionHandler:nil];
                }
            }

            uint64_t vid = g_next_vid++;
            ViewEntry *entry = calloc(1, sizeof(ViewEntry));
            entry->webview = wv;
            entry->window = win;
            g_views[@(vid)] = [NSValue valueWithPointer:entry];

            write_frame(req_id, REP_VIEW_CREATED, &vid, 8);
            break;
        }

        case OP_LIST_VIEWS: {
            NSArray *keys = [g_views allKeys];
            uint32_t count = (uint32_t)keys.count;
            uint32_t total = 4 + count * 8;
            uint8_t *buf = malloc(total);
            memcpy(buf, &count, 4);
            for (uint32_t i = 0; i < count; i++) {
                uint64_t vid = [keys[i] unsignedLongLongValue];
                memcpy(buf + 4 + i * 8, &vid, 8);
            }
            write_frame(req_id, REP_VIEW_LIST, buf, total);
            free(buf);
            break;
        }

        case OP_NAVIGATE: {
            uint32_t off = 0;
            NSString *url = read_str(payload, payload_len, &off);
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) {
                NSURL *nsurl = [NSURL URLWithString:url];
                if (nsurl) {
                    [v->webview loadRequest:[NSURLRequest requestWithURL:nsurl]];
                    write_frame(req_id, REP_OK, NULL, 0);
                } else {
                    write_frame_str(req_id, REP_ERROR, @"bad URL");
                }
            } else {
                write_frame_str(req_id, REP_ERROR, @"no view");
            }
            break;
        }

        case OP_WAIT_NAV: {
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v && v->webview.isLoading) {
                // Register waiter — nav delegate fires it on didFinish/didFail
                uint32_t captured_rid = req_id;
                g_nav_delegate.waiters[@((uintptr_t)v->webview)] = ^(NSError *err) {
                    if (err) {
                        write_frame_str(captured_rid, REP_ERROR,
                                       err.localizedDescription);
                    } else {
                        write_frame(captured_rid, REP_OK, NULL, 0);
                    }
                };
                // Don't write response here — waiter callback writes it
            } else {
                write_frame(req_id, REP_OK, NULL, 0);
            }
            break;
        }

        case OP_RELOAD: {
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) [v->webview reloadFromOrigin];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        // Go back/forward — native WKWebView methods, no JS evaluation.
        // Op codes 7 (GoBack) and 8 (GoForward) match Bun's ipc_protocol.h.
        case 7: { // GoBack
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v && [v->webview canGoBack]) [v->webview goBack];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }
        case 8: { // GoForward
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v && [v->webview canGoForward]) [v->webview goForward];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_GET_URL: {
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            NSString *url = v ? (v->webview.URL.absoluteString ?: @"about:blank") : @"";
            NSString *json = [NSString stringWithFormat:@"\"%@\"",
                [url stringByReplacingOccurrencesOfString:@"\"" withString:@"\\\""]];
            write_frame_str(req_id, REP_VALUE, json);
            break;
        }

        case OP_GET_TITLE: {
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            NSString *title = v ? (v->webview.title ?: @"") : @"";
            NSString *json = [NSString stringWithFormat:@"\"%@\"",
                [title stringByReplacingOccurrencesOfString:@"\"" withString:@"\\\""]];
            write_frame_str(req_id, REP_VALUE, json);
            break;
        }

        case OP_EVALUATE: {
            uint32_t off = 0;
            NSString *expr = read_str(payload, payload_len, &off);
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            uint32_t captured_rid = req_id;

            // callAsyncJavaScript wraps the body in an async function, so we can use
            // `return`. We use eval() to handle both expressions and multi-statement code.
            // eval() returns the last expression value for expressions, and runs
            // multi-statement code correctly. JSON.stringify captures the result.
            NSString *escaped = [expr stringByReplacingOccurrencesOfString:@"\\" withString:@"\\\\"];
            escaped = [escaped stringByReplacingOccurrencesOfString:@"`" withString:@"\\`"];
            NSString *body = [NSString stringWithFormat:@"return JSON.stringify(await eval(`%@`))", escaped];
            [v->webview callAsyncJavaScript:body
                                 arguments:nil
                                   inFrame:nil
                            inContentWorld:[WKContentWorld pageWorld]
                         completionHandler:^(id result, NSError *error) {
                if (error) {
                    // Extract the actual exception message from userInfo if available
                    NSString *msg = error.userInfo[@"WKJavaScriptExceptionMessage"];
                    if (!msg) msg = error.localizedDescription;
                    write_frame_str(captured_rid, REP_ERROR, msg);
                    return;
                }
                // result is NSString (JSON.stringify output) or nil (undefined)
                if (!result || [result isKindOfClass:[NSNull class]]) {
                    write_frame_str(captured_rid, REP_VALUE, @"null");
                } else if ([result isKindOfClass:[NSString class]]) {
                    // result IS the JSON string already (from JSON.stringify)
                    NSString *s = (NSString *)result;
                    if (s.length == 0) {
                        // JSON.stringify(undefined) returns undefined -> nil,
                        // but empty string means JSON.stringify returned ""
                        write_frame_str(captured_rid, REP_VALUE, @"null");
                    } else {
                        // The string IS JSON — pass it through directly
                        write_frame_str(captured_rid, REP_VALUE, s);
                    }
                } else {
                    // Shouldn't happen with JSON.stringify, but handle gracefully
                    write_frame_str(captured_rid, REP_VALUE, @"null");
                }
            }];
            // Async — completion fires via CFRunLoop
            break;
        }

        case OP_SCREENSHOT: {
            uint32_t off = 0;
            // Payload: u8 format (0=png, 1=jpeg, 2=webp) + u8 quality + u64 vid
            uint8_t img_format = (off < payload_len) ? payload[off++] : 0;
            uint8_t img_quality = (off < payload_len) ? payload[off++] : 80;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            uint8_t captured_format = img_format;
            uint8_t captured_quality = img_quality;

            // Helper: encode CGImage and send via shared memory
            void (^encodeCGImageAndSend)(CGImageRef, uint32_t) = ^(CGImageRef cg, uint32_t rid) {
                if (!cg) { write_frame_str(rid, REP_ERROR, @"no CGImage"); return; }

                CFMutableDataRef imgData;
                CFStringRef utType = (captured_format == 1) ? (__bridge CFStringRef)UTTypeJPEG.identifier : (__bridge CFStringRef)UTTypePNG.identifier;
                imgData = CFDataCreateMutable(NULL, 0);
                CGImageDestinationRef dest = CGImageDestinationCreateWithData(imgData, utType, 1, NULL);
                if (!dest) { CFRelease(imgData); write_frame_str(rid, REP_ERROR, @"encoder fail"); return; }
                if (captured_format == 1) {
                    float q = (float)captured_quality / 100.0f;
                    NSDictionary *props = @{(__bridge NSString *)kCGImageDestinationLossyCompressionQuality: @(q)};
                    CGImageDestinationAddImage(dest, cg, (__bridge CFDictionaryRef)props);
                } else {
                    CGImageDestinationAddImage(dest, cg, NULL);
                }
                CGImageDestinationFinalize(dest);
                CFRelease(dest);

                unsigned long dataLen = (unsigned long)CFDataGetLength(imgData);
                const uint8_t *dataBytes = CFDataGetBytePtr(imgData);

                // Shared memory transfer
                static uint32_t shm_seq = 0;
                char name[64];
                snprintf(name, sizeof(name), "/fd-wk-%d-%u", getpid(), ++shm_seq);
                int shm_fd = shm_open(name, O_CREAT | O_RDWR | O_EXCL, 0600);
                if (shm_fd < 0) {
                    write_frame(rid, REP_BINARY, dataBytes, (uint32_t)dataLen);
                    CFRelease(imgData);
                    return;
                }
                if (ftruncate(shm_fd, (off_t)dataLen) != 0) {
                    close(shm_fd); shm_unlink(name);
                    write_frame(rid, REP_BINARY, dataBytes, (uint32_t)dataLen);
                    CFRelease(imgData);
                    return;
                }
                void *map = mmap(NULL, dataLen, PROT_READ | PROT_WRITE, MAP_SHARED, shm_fd, 0);
                close(shm_fd);
                if (map == MAP_FAILED) {
                    shm_unlink(name);
                    write_frame(rid, REP_BINARY, dataBytes, (uint32_t)dataLen);
                    CFRelease(imgData);
                    return;
                }
                memcpy(map, dataBytes, dataLen);
                munmap(map, dataLen);
                CFRelease(imgData);

                uint32_t nameLen = (uint32_t)strlen(name);
                uint32_t total = 4 + nameLen + 4;
                uint8_t *buf = malloc(total);
                memcpy(buf, &nameLen, 4);
                memcpy(buf + 4, name, nameLen);
                memcpy(buf + 4 + nameLen, &dataLen, 4);
                write_frame(rid, REP_SHM_SCREENSHOT, buf, total);
                free(buf);
            };

            uint32_t captured_rid = req_id;

            // CGWindowListCreateImage was removed in macOS 15 SDK.
            // takeSnapshotWithConfiguration is the only supported path.
            WKSnapshotConfiguration *cfg = [[WKSnapshotConfiguration alloc] init];
            cfg.afterScreenUpdates = YES;

            [v->webview takeSnapshotWithConfiguration:cfg
                completionHandler:^(NSImage *image, NSError *error) {
                if (error || !image) {
                    write_frame_str(captured_rid, REP_ERROR,
                        error ? error.localizedDescription : @"no image");
                    return;
                }
                CGImageRef cg = [image CGImageForProposedRect:NULL context:nil hints:nil];
                encodeCGImageAndSend(cg, captured_rid);
            }];
            break;
        }

        case OP_CLICK: {
            // Native NSEvent mouse dispatch — same as Bun's doNativeClick.
            // Sends mouseDown/mouseUp directly to WKWebView, producing
            // isTrusted:true events. Uses _doAfterProcessingAllPendingMouseEvents:
            // as completion barrier if available.
            uint32_t off = 0;
            double x = 0, y = 0;
            if (off + 8 <= payload_len) { memcpy(&x, payload + off, 8); off += 8; }
            if (off + 8 <= payload_len) { memcpy(&y, payload + off, 8); off += 8; }
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) {
                // viewport y-down -> window y-up
                double wy = CGRectGetHeight(v->webview.bounds) - y;
                NSTimeInterval ts = [NSProcessInfo processInfo].systemUptime;
                NSInteger winNum = [v->window windowNumber];

                NSEvent *down = [NSEvent mouseEventWithType:NSEventTypeLeftMouseDown
                    location:NSMakePoint(x, wy)
                    modifierFlags:0 timestamp:ts
                    windowNumber:winNum context:nil
                    eventNumber:0 clickCount:1 pressure:1.0];
                NSEvent *up = [NSEvent mouseEventWithType:NSEventTypeLeftMouseUp
                    location:NSMakePoint(x, wy)
                    modifierFlags:0 timestamp:ts
                    windowNumber:winNum context:nil
                    eventNumber:0 clickCount:1 pressure:1.0];

                [v->webview mouseDown:down];
                [v->webview mouseUp:up];

                // Completion barrier — waits for mouseEventQueue to drain
                SEL barrierSel = NSSelectorFromString(@"_doAfterProcessingAllPendingMouseEvents:");
                if ([v->webview respondsToSelector:barrierSel]) {
                    uint32_t captured_rid = req_id;
                    void (^block)(void) = ^{
                        write_frame(captured_rid, REP_OK, NULL, 0);
                    };
                    ((void(*)(id,SEL,id))objc_msgSend)(v->webview, barrierSel, block);
                } else {
                    write_frame(req_id, REP_OK, NULL, 0);
                }
            } else {
                write_frame(req_id, REP_OK, NULL, 0);
            }
            break;
        }

        case OP_TYPE: {
            // Native _executeEditCommand:argument:completion: — same as Bun's typeIPC.
            // InsertText editing command, fires beforeinput/input, isTrusted:true.
            uint32_t off = 0;
            NSString *text = read_str(payload, payload_len, &off);
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) {
                SEL execSel = NSSelectorFromString(@"_executeEditCommand:argument:completion:");
                if ([v->webview respondsToSelector:execSel]) {
                    uint32_t captured_rid = req_id;
                    void (^block)(BOOL) = ^(BOOL success) {
                        (void)success;
                        write_frame(captured_rid, REP_OK, NULL, 0);
                    };
                    ((void(*)(id,SEL,id,id,id))objc_msgSend)(
                        v->webview, execSel,
                        @"InsertText", text, block);
                } else {
                    // Fallback to JS
                    NSString *escaped = [[text stringByReplacingOccurrencesOfString:@"\\" withString:@"\\\\"]
                        stringByReplacingOccurrencesOfString:@"'" withString:@"\\'"];
                    NSString *js = [NSString stringWithFormat:
                        @"(function(){var e=document.activeElement;if(!e)return;"
                        "var t='%@';for(var i=0;i<t.length;i++){var c=t[i];"
                        "if(e.tagName==='INPUT'||e.tagName==='TEXTAREA')e.value+=c;"
                        "e.dispatchEvent(new Event('input',{bubbles:true}))}})()", escaped];
                    [v->webview evaluateJavaScript:js completionHandler:nil];
                    write_frame(req_id, REP_OK, NULL, 0);
                }
            } else {
                write_frame(req_id, REP_OK, NULL, 0);
            }
            break;
        }

        case OP_PRESS_KEY:
        case OP_KEY_DOWN:
        case OP_KEY_UP: {
            // Dispatch native NSEvent keyDown and/or keyUp.
            // OP_PRESS_KEY sends both, OP_KEY_DOWN sends only keyDown, OP_KEY_UP sends only keyUp.
            uint32_t off = 0;
            NSString *key = read_str(payload, payload_len, &off);
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) {
                NSTimeInterval ts = [NSProcessInfo processInfo].systemUptime;
                NSInteger winNum = [v->window windowNumber];

                // Map key name to character + keyCode for NSEvent
                NSString *chars = key;
                uint16_t keyCode = 0;
                NSUInteger modFlags = 0;
                if ([key isEqualToString:@"Enter"])       { chars = @"\r"; keyCode = 0x24; }
                else if ([key isEqualToString:@"Tab"])     { chars = @"\t"; keyCode = 0x30; }
                else if ([key isEqualToString:@"Backspace"]) { chars = [NSString stringWithFormat:@"%C", (unichar)0x08]; keyCode = 0x33; }
                else if ([key isEqualToString:@"Delete"])  { chars = [NSString stringWithFormat:@"%C", (unichar)0x7F]; keyCode = 0x75; }
                else if ([key isEqualToString:@"Escape"])  { chars = [NSString stringWithFormat:@"%C", (unichar)0x1B]; keyCode = 0x35; }
                else if ([key isEqualToString:@"Space"])   { chars = @" "; keyCode = 0x31; }
                else if ([key isEqualToString:@"ArrowLeft"])  { chars = [NSString stringWithFormat:@"%C", (unichar)0xF702]; keyCode = 0x7B; }
                else if ([key isEqualToString:@"ArrowRight"]) { chars = [NSString stringWithFormat:@"%C", (unichar)0xF703]; keyCode = 0x7C; }
                else if ([key isEqualToString:@"ArrowDown"])  { chars = [NSString stringWithFormat:@"%C", (unichar)0xF701]; keyCode = 0x7D; }
                else if ([key isEqualToString:@"ArrowUp"])    { chars = [NSString stringWithFormat:@"%C", (unichar)0xF700]; keyCode = 0x7E; }
                else if ([key isEqualToString:@"Home"])    { chars = [NSString stringWithFormat:@"%C", (unichar)0xF729]; keyCode = 0x73; }
                else if ([key isEqualToString:@"End"])     { chars = [NSString stringWithFormat:@"%C", (unichar)0xF72B]; keyCode = 0x77; }
                else if ([key isEqualToString:@"PageUp"])  { chars = [NSString stringWithFormat:@"%C", (unichar)0xF72C]; keyCode = 0x74; }
                else if ([key isEqualToString:@"PageDown"])  { chars = [NSString stringWithFormat:@"%C", (unichar)0xF72D]; keyCode = 0x79; }
                else if ([key isEqualToString:@"Shift"] || [key isEqualToString:@"ShiftLeft"] || [key isEqualToString:@"ShiftRight"])
                  { chars = @""; keyCode = 0x38; modFlags = NSEventModifierFlagShift; }
                else if ([key isEqualToString:@"Control"] || [key isEqualToString:@"ControlLeft"] || [key isEqualToString:@"ControlRight"])
                  { chars = @""; keyCode = 0x3B; modFlags = NSEventModifierFlagControl; }
                else if ([key isEqualToString:@"Alt"] || [key isEqualToString:@"AltLeft"] || [key isEqualToString:@"AltRight"])
                  { chars = @""; keyCode = 0x3A; modFlags = NSEventModifierFlagOption; }
                else if ([key isEqualToString:@"Meta"] || [key isEqualToString:@"MetaLeft"] || [key isEqualToString:@"MetaRight"])
                  { chars = @""; keyCode = 0x37; modFlags = NSEventModifierFlagCommand; }
                else if (key.length == 1) { chars = key; }

                if (op != OP_KEY_UP) {
                    NSEvent *down = [NSEvent keyEventWithType:NSEventTypeKeyDown
                        location:NSZeroPoint modifierFlags:modFlags
                        timestamp:ts windowNumber:winNum
                        context:nil characters:chars
                        charactersIgnoringModifiers:chars
                        isARepeat:NO keyCode:keyCode];
                    [v->webview keyDown:down];
                }
                if (op != OP_KEY_DOWN) {
                    NSEvent *up = [NSEvent keyEventWithType:NSEventTypeKeyUp
                        location:NSZeroPoint modifierFlags:modFlags
                        timestamp:ts windowNumber:winNum
                        context:nil characters:chars
                        charactersIgnoringModifiers:chars
                        isARepeat:NO keyCode:keyCode];
                    [v->webview keyUp:up];
                }
                write_frame(req_id, REP_OK, NULL, 0);
            } else {
                write_frame(req_id, REP_OK, NULL, 0);
            }
            break;
        }

        case OP_SET_USER_AGENT: {
            uint32_t off = 0;
            NSString *ua = read_str(payload, payload_len, &off);
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) [v->webview setCustomUserAgent:ua];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_SET_FILE_INPUT: {
            // Set file on <input type="file"> via DataTransfer API.
            // Payload: str selector + str filePath + u64 viewId
            uint32_t off = 0;
            NSString *selector = read_str(payload, payload_len, &off);
            NSString *filePath = read_str(payload, payload_len, &off);
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v || !selector || !filePath) {
                write_frame_str(req_id, REP_ERROR, @"invalid args");
                break;
            }

            // Read file from disk
            NSData *fileData = [NSData dataWithContentsOfFile:filePath];
            if (!fileData) {
                write_frame_str(req_id, REP_ERROR,
                    [NSString stringWithFormat:@"File not found: %@", filePath]);
                break;
            }

            // Base64 encode file content for JS injection
            NSString *b64 = [fileData base64EncodedStringWithOptions:0];
            NSString *fileName = [filePath lastPathComponent];

            // Detect MIME type from extension
            NSString *ext = [[filePath pathExtension] lowercaseString];
            NSString *mime = @"application/octet-stream";
            if ([ext isEqualToString:@"txt"]) mime = @"text/plain";
            else if ([ext isEqualToString:@"html"] || [ext isEqualToString:@"htm"]) mime = @"text/html";
            else if ([ext isEqualToString:@"json"]) mime = @"application/json";
            else if ([ext isEqualToString:@"pdf"]) mime = @"application/pdf";
            else if ([ext isEqualToString:@"png"]) mime = @"image/png";
            else if ([ext isEqualToString:@"jpg"] || [ext isEqualToString:@"jpeg"]) mime = @"image/jpeg";
            else if ([ext isEqualToString:@"gif"]) mime = @"image/gif";
            else if ([ext isEqualToString:@"svg"]) mime = @"image/svg+xml";
            else if ([ext isEqualToString:@"csv"]) mime = @"text/csv";
            else if ([ext isEqualToString:@"xml"]) mime = @"application/xml";
            else if ([ext isEqualToString:@"zip"]) mime = @"application/zip";

            // JS: decode base64, create File, assign via DataTransfer
            NSString *js = [NSString stringWithFormat:
                @"(function(){"
                "var el=document.querySelector('%@');"
                "if(!el)return 'not found';"
                "var b64='%@';"
                "var bytes=atob(b64);"
                "var arr=new Uint8Array(bytes.length);"
                "for(var i=0;i<bytes.length;i++)arr[i]=bytes.charCodeAt(i);"
                "var file=new File([arr],'%@',{type:'%@'});"
                "var dt=new DataTransfer();"
                "dt.items.add(file);"
                "el.files=dt.files;"
                "el.dispatchEvent(new Event('change',{bubbles:true}));"
                "return 'ok';"
                "})()",
                [selector stringByReplacingOccurrencesOfString:@"'" withString:@"\\'"],
                b64, fileName, mime];

            uint32_t captured_rid = req_id;
            [v->webview evaluateJavaScript:js completionHandler:^(id result, NSError *err) {
                if (err) {
                    write_frame_str(captured_rid, REP_ERROR,
                        [NSString stringWithFormat:@"JS error: %@", err.localizedDescription]);
                } else {
                    NSString *r = [NSString stringWithFormat:@"%@", result ?: @""];
                    if ([r isEqualToString:@"not found"]) {
                        write_frame_str(captured_rid, REP_ERROR, @"Element not found");
                    } else {
                        write_frame(captured_rid, REP_OK, NULL, 0);
                    }
                }
            }];
            break;
        }

        case OP_SET_VIEWPORT: {
            // Payload: f64 width + f64 height + f64 deviceScaleFactor + u64 viewId
            uint32_t off = 0;
            double w = 0, h = 0, scale = 1.0;
            if (off + 8 <= payload_len) { memcpy(&w, payload + off, 8); off += 8; }
            if (off + 8 <= payload_len) { memcpy(&h, payload + off, 8); off += 8; }
            if (off + 8 <= payload_len) { memcpy(&scale, payload + off, 8); off += 8; }
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (v) {
                // Use WKWebView's private _overrideDeviceScaleFactor for REAL DPR emulation.
                // This affects actual rendering -- text rasterization, image selection,
                // canvas backing stores. Not just a JS property override.
                SEL dprSel = NSSelectorFromString(@"_setOverrideDeviceScaleFactor:");
                if ([v->webview respondsToSelector:dprSel]) {
                    ((void(*)(id,SEL,CGFloat))objc_msgSend)(v->webview, dprSel, (CGFloat)scale);
                } else {
                    // Fallback: set window backing scale factor
                    ((FDHostWindow *)v->window).emulatedScaleFactor = (CGFloat)scale;
                }

                // Resize window and webview
                NSRect frame = NSMakeRect(0, 0, w, h);
                [v->window setFrame:frame display:YES];
                [v->webview setFrame:NSMakeRect(0, 0, w, h)];

                // Override screen.width/screen.height via WKUserScript (persists across navigations).
                // No native API on Cocoa -- WebKit compiles out Page.setScreenSizeOverride.
                NSString *screenJS = [NSString stringWithFormat:
                    @"(function(){if(window.__fd_screen)return;window.__fd_screen=1;"
                    "Object.defineProperty(screen,'width',{get:function(){return %d},configurable:true});"
                    "Object.defineProperty(screen,'height',{get:function(){return %d},configurable:true});"
                    "Object.defineProperty(screen,'availWidth',{get:function(){return %d},configurable:true});"
                    "Object.defineProperty(screen,'availHeight',{get:function(){return %d},configurable:true})})()",
                    (int)w, (int)h, (int)w, (int)h];
                WKUserScript *screenScript = [[WKUserScript alloc]
                    initWithSource:screenJS
                    injectionTime:WKUserScriptInjectionTimeAtDocumentStart
                    forMainFrameOnly:NO];
                // Remove any previous screen override script and re-add
                // (WKUserContentController doesn't support removing individual scripts,
                // but addUserScript appends -- old ones still run first, our idempotency
                // guard __fd_screen prevents double-execution. New navigations get the
                // latest values since all scripts re-run at document start.)
                [v->webview.configuration.userContentController addUserScript:screenScript];
                // Also run immediately on the current page
                [v->webview evaluateJavaScript:screenJS completionHandler:nil];
            }
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_GET_COOKIES: {
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            uint32_t captured_rid = req_id;
            WKHTTPCookieStore *store = v->webview.configuration.websiteDataStore.httpCookieStore;
            [store getAllCookies:^(NSArray<NSHTTPCookie *> *cookies) {
                NSMutableArray *arr = [NSMutableArray new];
                for (NSHTTPCookie *c in cookies) {
                    [arr addObject:@{
                        @"name": c.name ?: @"",
                        @"value": c.value ?: @"",
                        @"domain": c.domain ?: @"",
                        @"path": c.path ?: @"/",
                        @"secure": @(c.isSecure),
                        @"http_only": @(c.isHTTPOnly),
                        @"expires": c.expiresDate ? @([c.expiresDate timeIntervalSince1970]) : [NSNull null],
                    }];
                }
                NSData *json = [NSJSONSerialization dataWithJSONObject:arr options:0 error:nil];
                NSString *s = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
                write_frame_str(captured_rid, REP_VALUE, s ?: @"[]");
            }];
            break;
        }

        case OP_SET_COOKIE: {
            // Payload: u64 vid + str name + str value + str domain + str path + u8 secure + u8 httpOnly + f64 expires (-1 = session)
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *name = read_str(payload, payload_len, &off);
            NSString *value = read_str(payload, payload_len, &off);
            NSString *domain = read_str(payload, payload_len, &off);
            NSString *path = read_str(payload, payload_len, &off);
            uint8_t secure = (off < payload_len) ? payload[off++] : 0;
            uint8_t httpOnly = (off < payload_len) ? payload[off++] : 0;
            double expires = -1;
            if (off + 8 <= payload_len) {
                memcpy(&expires, payload + off, 8); off += 8;
            }

            NSMutableDictionary *props = [NSMutableDictionary dictionaryWithDictionary:@{
                NSHTTPCookieName: name,
                NSHTTPCookieValue: value,
                NSHTTPCookieDomain: domain,
                NSHTTPCookiePath: path.length > 0 ? path : @"/",
            }];
            if (secure) props[NSHTTPCookieSecure] = @"TRUE";
            if (httpOnly) props[@"HttpOnly"] = @"TRUE";
            if (expires > 0) {
                props[NSHTTPCookieExpires] = [NSDate dateWithTimeIntervalSince1970:expires];
            }

            NSHTTPCookie *cookie = [NSHTTPCookie cookieWithProperties:props];
            if (!cookie) { write_frame_str(req_id, REP_ERROR, @"invalid cookie"); break; }

            uint32_t captured_rid = req_id;
            WKHTTPCookieStore *store = v->webview.configuration.websiteDataStore.httpCookieStore;
            [store setCookie:cookie completionHandler:^{
                write_frame(captured_rid, REP_OK, NULL, 0);
            }];
            break;
        }

        case OP_DELETE_COOKIE: {
            // Payload: u64 vid + str name + str domain
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *name = read_str(payload, payload_len, &off);
            NSString *domain = read_str(payload, payload_len, &off);

            uint32_t captured_rid = req_id;
            WKHTTPCookieStore *store = v->webview.configuration.websiteDataStore.httpCookieStore;
            [store getAllCookies:^(NSArray<NSHTTPCookie *> *cookies) {
                __block int pending = 0;
                __block BOOL any = NO;
                for (NSHTTPCookie *c in cookies) {
                    if (![c.name isEqualToString:name]) continue;
                    if (domain.length > 0 && ![c.domain isEqualToString:domain]) continue;
                    any = YES;
                    pending++;
                    [store deleteCookie:c completionHandler:^{
                        if (--pending == 0) {
                            write_frame(captured_rid, REP_OK, NULL, 0);
                        }
                    }];
                }
                if (!any) write_frame(captured_rid, REP_OK, NULL, 0);
            }];
            break;
        }

        case OP_CLEAR_COOKIES: {
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            uint32_t captured_rid = req_id;
            WKWebsiteDataStore *store = v->webview.configuration.websiteDataStore;
            NSSet *types = [NSSet setWithObject:WKWebsiteDataTypeCookies];
            [store removeDataOfTypes:types
                   modifiedSince:[NSDate distantPast]
                   completionHandler:^{
                write_frame(captured_rid, REP_OK, NULL, 0);
            }];
            break;
        }

        case OP_LOAD_HTML: {
            // Payload: u64 vid + str html + str baseURL
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *html = read_str(payload, payload_len, &off);
            NSString *base = read_str(payload, payload_len, &off);
            NSURL *baseURL = base.length > 0 ? [NSURL URLWithString:base] : nil;

            // Register nav waiter so caller can wait for load completion
            uint32_t captured_rid = req_id;
            g_nav_delegate.waiters[@((uintptr_t)v->webview)] = ^(NSError *err) {
                if (err) {
                    write_frame_str(captured_rid, REP_ERROR,
                        err.localizedDescription ?: @"load failed");
                } else {
                    write_frame(captured_rid, REP_OK, NULL, 0);
                }
            };

            [v->webview loadHTMLString:html baseURL:baseURL];
            break;
        }

        case OP_ADD_INIT_SCRIPT: {
            // Payload: u64 vid + str source
            // Adds a WKUserScript that runs at document start on every navigation.
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *source = read_str(payload, payload_len, &off);
            WKUserScript *script = [[WKUserScript alloc]
                initWithSource:source
                injectionTime:WKUserScriptInjectionTimeAtDocumentStart
                forMainFrameOnly:YES];
            [v->webview.configuration.userContentController addUserScript:script];

            // Also run immediately on the current page
            [v->webview evaluateJavaScript:source completionHandler:nil];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_MOUSE_EVENT: {
            // Generic native mouse event dispatch.
            // Payload: u8 type(0=move,1=down,2=up) + u8 button(0=left,1=right,2=middle) + u32 clickCount + f64 x + f64 y + u64 vid
            uint32_t off = 0;
            uint8_t mouse_type = (off < payload_len) ? payload[off++] : 0;
            uint8_t mouse_button = (off < payload_len) ? payload[off++] : 0;
            uint32_t click_count = 1;
            if (off + 4 <= payload_len) { memcpy(&click_count, payload + off, 4); off += 4; }
            double x = 0, y = 0;
            if (off + 8 <= payload_len) { memcpy(&x, payload + off, 8); off += 8; }
            if (off + 8 <= payload_len) { memcpy(&y, payload + off, 8); off += 8; }
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame(req_id, REP_OK, NULL, 0); break; }

            double wy = CGRectGetHeight(v->webview.bounds) - y;
            NSTimeInterval ts = [NSProcessInfo processInfo].systemUptime;
            NSInteger winNum = [v->window windowNumber];

            // Map button + type to NSEventType
            NSEventType evType;
            if (mouse_type == 0) {
                // Move
                evType = (mouse_button == 0) ? NSEventTypeMouseMoved : NSEventTypeMouseMoved;
            } else if (mouse_type == 1) {
                // Down
                switch (mouse_button) {
                    case 1: evType = NSEventTypeRightMouseDown; break;
                    case 2: evType = NSEventTypeOtherMouseDown; break;
                    default: evType = NSEventTypeLeftMouseDown; break;
                }
            } else {
                // Up
                switch (mouse_button) {
                    case 1: evType = NSEventTypeRightMouseUp; break;
                    case 2: evType = NSEventTypeOtherMouseUp; break;
                    default: evType = NSEventTypeLeftMouseUp; break;
                }
            }

            NSEvent *ev = [NSEvent mouseEventWithType:evType
                location:NSMakePoint(x, wy)
                modifierFlags:0 timestamp:ts
                windowNumber:winNum context:nil
                eventNumber:0 clickCount:(NSInteger)click_count pressure:(mouse_type == 1 ? 1.0 : 0.0)];

            // Temporarily allow mouse events so sendEvent: works
            BOOL wasIgnoring = [v->window ignoresMouseEvents];
            if (wasIgnoring) [v->window setIgnoresMouseEvents:NO];

            // Use sendEvent: for proper event pipeline propagation to web content
            [v->window sendEvent:ev];

            if (wasIgnoring) [v->window setIgnoresMouseEvents:YES];

            // Completion barrier for mouse up events
            if (mouse_type == 2) {
                SEL barrierSel = NSSelectorFromString(@"_doAfterProcessingAllPendingMouseEvents:");
                if ([v->webview respondsToSelector:barrierSel]) {
                    uint32_t captured_rid = req_id;
                    void (^block)(void) = ^{
                        write_frame(captured_rid, REP_OK, NULL, 0);
                    };
                    ((void(*)(id,SEL,id))objc_msgSend)(v->webview, barrierSel, block);
                } else {
                    write_frame(req_id, REP_OK, NULL, 0);
                }
            } else {
                write_frame(req_id, REP_OK, NULL, 0);
            }
            break;
        }

        case OP_SET_LOCALE: {
            // Inject locale override at document start via WKUserScript.
            // This runs before any page JS, overriding navigator.language/languages
            // across all rendering pipelines (Intl, toLocaleString, etc.)
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *locale = read_str(payload, payload_len, &off);
            NSString *js = [NSString stringWithFormat:
                @"Object.defineProperty(navigator,'language',{get:function(){return '%@'},configurable:true});"
                "Object.defineProperty(navigator,'languages',{get:function(){return ['%@']},configurable:true});"
                "if(typeof Intl!=='undefined'){var _DT=Intl.DateTimeFormat;Intl.DateTimeFormat=function(l,o){"
                "return new _DT('%@',o)};Intl.DateTimeFormat.prototype=_DT.prototype;"
                "var _NF=Intl.NumberFormat;Intl.NumberFormat=function(l,o){"
                "return new _NF('%@',o)};Intl.NumberFormat.prototype=_NF.prototype}",
                locale, locale, locale, locale];
            WKUserScript *script = [[WKUserScript alloc]
                initWithSource:js injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO];
            [v->webview.configuration.userContentController addUserScript:script];
            [v->webview evaluateJavaScript:js completionHandler:nil];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_SET_TIMEZONE: {
            // Inject timezone override at document start via WKUserScript.
            // Overrides Date.prototype.toLocaleString and Intl.DateTimeFormat.
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *tz = read_str(payload, payload_len, &off);
            NSString *js = [NSString stringWithFormat:
                @"(function(){"
                "var _DTF=Intl.DateTimeFormat;"
                "Intl.DateTimeFormat=function(l,o){o=Object.assign({},o);o.timeZone='%@';return new _DTF(l,o)};"
                "Intl.DateTimeFormat.prototype=_DTF.prototype;"
                "Intl.DateTimeFormat.supportedLocalesOf=_DTF.supportedLocalesOf;"
                "var _tls=Date.prototype.toLocaleString;"
                "Date.prototype.toLocaleString=function(l,o){o=Object.assign({},o);o.timeZone='%@';return _tls.call(this,l,o)};"
                "var _tds=Date.prototype.toLocaleDateString;"
                "Date.prototype.toLocaleDateString=function(l,o){o=Object.assign({},o);o.timeZone='%@';return _tds.call(this,l,o)};"
                "var _tts=Date.prototype.toLocaleTimeString;"
                "Date.prototype.toLocaleTimeString=function(l,o){o=Object.assign({},o);o.timeZone='%@';return _tts.call(this,l,o)};"
                "})()", tz, tz, tz, tz];
            WKUserScript *script = [[WKUserScript alloc]
                initWithSource:js injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO];
            [v->webview.configuration.userContentController addUserScript:script];
            [v->webview evaluateJavaScript:js completionHandler:nil];
            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_EMULATE_MEDIA: {
            // Emulate media features. Uses WKWebView's _setForcedAppearance for dark mode
            // (native rendering pipeline), WKUserScript for other features.
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            ViewEntry *v = get_view(vid);
            if (!v) { write_frame_str(req_id, REP_ERROR, @"no view"); break; }

            NSString *colorScheme = read_str(payload, payload_len, &off);
            NSString *reducedMotion = read_str(payload, payload_len, &off);
            NSString *forcedColors = read_str(payload, payload_len, &off);
            NSString *media = read_str(payload, payload_len, &off);
            NSString *contrast = read_str(payload, payload_len, &off);

            // Use private API for dark mode if available (affects CSS media queries natively)
            if (colorScheme.length > 0) {
                SEL appearanceSel = NSSelectorFromString(@"_setOverrideAppearance:");
                if ([v->webview respondsToSelector:appearanceSel]) {
                    NSAppearance *appearance = nil;
                    if ([colorScheme isEqualToString:@"dark"]) {
                        appearance = [NSAppearance appearanceNamed:NSAppearanceNameDarkAqua];
                    } else if ([colorScheme isEqualToString:@"light"]) {
                        appearance = [NSAppearance appearanceNamed:NSAppearanceNameAqua];
                    }
                    ((void(*)(id,SEL,id))objc_msgSend)(v->webview, appearanceSel, appearance);
                }
            }

            // Use native setMediaType: for media type emulation (screen/print)
            if (media.length > 0) {
                [v->webview setMediaType:media];
            }
            if (reducedMotion.length > 0) {
                // Intercept matchMedia to override prefers-reduced-motion.
                // The native MediaQueryList.matches is read-only, so we wrap matchMedia.
                NSString *val = [reducedMotion isEqualToString:@"reduce"] ? @"reduce" : @"no-preference";
                NSString *js = [NSString stringWithFormat:
                    @"(function(){"
                    "var _mm=window.matchMedia;"
                    "window.matchMedia=function(q){"
                    "var r=_mm.call(window,q);"
                    "if(q.indexOf('prefers-reduced-motion')!==-1){"
                    "var m=q.indexOf('reduce')!==-1;"
                    "var want=%@;"
                    "return Object.create(r,{matches:{get:function(){return want}}})}"
                    "return r}})()",
                    [val isEqualToString:@"reduce"] ? @"true" : @"false"];
                WKUserScript *script = [[WKUserScript alloc]
                    initWithSource:js injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO];
                [v->webview.configuration.userContentController addUserScript:script];
                [v->webview evaluateJavaScript:js completionHandler:nil];
            }

            // forced-colors: intercept matchMedia('(forced-colors: active)')
            if (forcedColors.length > 0) {
                BOOL isActive = [forcedColors isEqualToString:@"active"];
                NSString *js = [NSString stringWithFormat:
                    @"(function(){"
                    "if(!window.__fd_mm)window.__fd_mm=window.matchMedia;"
                    "var _mm=window.__fd_mm;"
                    "window.matchMedia=function(q){"
                    "var r=_mm.call(window,q);"
                    "if(q.indexOf('forced-colors')!==-1){"
                    "return Object.create(r,{matches:{get:function(){return %@}}})}"
                    "return r}})()",
                    isActive ? @"true" : @"false"];
                WKUserScript *script = [[WKUserScript alloc]
                    initWithSource:js injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO];
                [v->webview.configuration.userContentController addUserScript:script];
                [v->webview evaluateJavaScript:js completionHandler:nil];
            }

            // contrast: intercept matchMedia('(prefers-contrast: more)')
            if (contrast.length > 0) {
                BOOL isMore = [contrast isEqualToString:@"more"];
                NSString *js = [NSString stringWithFormat:
                    @"(function(){"
                    "if(!window.__fd_mm)window.__fd_mm=window.matchMedia;"
                    "var _mm=window.__fd_mm;"
                    "window.matchMedia=function(q){"
                    "var r=_mm.call(window,q);"
                    "if(q.indexOf('prefers-contrast')!==-1){"
                    "var m=q.indexOf('more')!==-1;"
                    "return Object.create(r,{matches:{get:function(){return %@}}})}"
                    "return r}})()",
                    isMore ? @"true" : @"false"];
                WKUserScript *script = [[WKUserScript alloc]
                    initWithSource:js injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO];
                [v->webview.configuration.userContentController addUserScript:script];
                [v->webview evaluateJavaScript:js completionHandler:nil];
            }

            write_frame(req_id, REP_OK, NULL, 0);
            break;
        }

        case OP_ACCESSIBILITY_TREE: {
            // Payload: u64 view_id + i32 depth
            uint32_t off = 0;
            uint64_t vid = read_u64(payload, payload_len, &off);
            int32_t maxDepth = -1;
            if (off + 4 <= payload_len) {
                memcpy(&maxDepth, payload + off, 4);
            }
            ViewEntry *v = get_view(vid);
            if (!v) {
                write_frame_str(req_id, REP_ERROR, @"no such view");
                break;
            }

            // Walk native NSAccessibility tree from the WKWebView.
            // In-process calls -- no accessibility permissions required.
            NSMutableArray *nodes = [NSMutableArray array];
            // Counter for unique node IDs
            __block int nodeCounter = 0;

            // Map from accessibility element pointer to nodeId string
            NSMapTable *elemToId = [NSMapTable strongToStrongObjectsMapTable];

            // Recursive block to walk the tree
            void (^__block walkTree)(id, NSString*, int);
            __weak __block void (^weakWalkTree)(id, NSString*, int);
            walkTree = ^(id elem, NSString *parentId, int depth) {
                if (maxDepth >= 0 && depth > maxDepth) return;
                if (![elem respondsToSelector:@selector(accessibilityRole)]) return;

                NSString *role = [elem accessibilityRole];
                if (!role) role = @"generic";

                // Map NSAccessibility roles to ARIA roles
                NSString *ariaRole = role;
                if ([role isEqualToString:NSAccessibilityButtonRole]) ariaRole = @"button";
                else if ([role isEqualToString:NSAccessibilityLinkRole]) ariaRole = @"link";
                else if ([role isEqualToString:NSAccessibilityTextFieldRole]) ariaRole = @"textbox";
                else if ([role isEqualToString:NSAccessibilityTextAreaRole]) ariaRole = @"textbox";
                else if ([role isEqualToString:NSAccessibilityCheckBoxRole]) ariaRole = @"checkbox";
                else if ([role isEqualToString:NSAccessibilityRadioButtonRole]) ariaRole = @"radio";
                else if ([role isEqualToString:NSAccessibilityPopUpButtonRole]) ariaRole = @"combobox";
                else if ([role isEqualToString:NSAccessibilityMenuItemRole]) ariaRole = @"menuitem";
                else if ([role isEqualToString:NSAccessibilityTabGroupRole]) ariaRole = @"tablist";
                else if ([role isEqualToString:@"AXTab"]) ariaRole = @"tab";
                else if ([role isEqualToString:NSAccessibilitySliderRole]) ariaRole = @"slider";
                else if ([role isEqualToString:NSAccessibilityImageRole]) ariaRole = @"img";
                else if ([role isEqualToString:@"AXHeading"]) ariaRole = @"heading";
                else if ([role isEqualToString:NSAccessibilityListRole]) ariaRole = @"list";
                else if ([role isEqualToString:NSAccessibilityTableRole]) ariaRole = @"table";
                else if ([role isEqualToString:NSAccessibilityRowRole]) ariaRole = @"row";
                else if ([role isEqualToString:NSAccessibilityCellRole]) ariaRole = @"cell";
                else if ([role isEqualToString:NSAccessibilityGroupRole]) ariaRole = @"group";
                else if ([role isEqualToString:NSAccessibilityToolbarRole]) ariaRole = @"toolbar";
                else if ([role isEqualToString:NSAccessibilityMenuRole]) ariaRole = @"menu";
                else if ([role isEqualToString:NSAccessibilityMenuBarRole]) ariaRole = @"menubar";
                else if ([role isEqualToString:NSAccessibilityStaticTextRole]) ariaRole = @"StaticText";
                else if ([role isEqualToString:@"AXWebArea"]) ariaRole = @"RootWebArea";
                else if ([role isEqualToString:@"AXLandmarkNavigation"]) ariaRole = @"navigation";
                else if ([role isEqualToString:@"AXLandmarkMain"]) ariaRole = @"main";
                else if ([role isEqualToString:@"AXLandmarkBanner"]) ariaRole = @"banner";
                else if ([role isEqualToString:@"AXLandmarkContentInfo"]) ariaRole = @"contentinfo";
                else if ([role isEqualToString:@"AXLandmarkComplementary"]) ariaRole = @"complementary";
                else if ([role isEqualToString:@"AXLandmarkSearch"]) ariaRole = @"search";
                else if ([role isEqualToString:NSAccessibilityComboBoxRole]) ariaRole = @"combobox";
                else if ([role isEqualToString:@"AXSearchField"]) ariaRole = @"searchbox";
                else if ([role isEqualToString:NSAccessibilityProgressIndicatorRole]) ariaRole = @"progressbar";
                else if ([role isEqualToString:NSAccessibilityDisclosureTriangleRole]) ariaRole = @"button";
                else if ([role isEqualToString:@"AXSwitch"]) ariaRole = @"switch";

                NSString *nodeId = [NSString stringWithFormat:@"n%d", nodeCounter++];
                [elemToId setObject:nodeId forKey:elem];

                // Accessible name: try label, title, description in order
                NSString *name = @"";
                if ([elem respondsToSelector:@selector(accessibilityLabel)]) {
                    NSString *lbl = [elem accessibilityLabel];
                    if (lbl.length > 0) name = lbl;
                }
                if (name.length == 0 && [elem respondsToSelector:@selector(accessibilityTitle)]) {
                    NSString *t = [elem accessibilityTitle];
                    if (t.length > 0) name = t;
                }

                NSString *desc = @"";
                if ([elem respondsToSelector:@selector(accessibilityHelp)]) {
                    NSString *h = [elem accessibilityHelp];
                    if (h.length > 0) desc = h;
                }

                // Properties
                NSMutableArray *props = [NSMutableArray array];

                // Level (for headings)
                if ([elem respondsToSelector:@selector(accessibilityDisclosureLevel)]) {
                    // Only include for headings - use undocumented attribute
                    if ([ariaRole isEqualToString:@"heading"]) {
                        id levelVal = nil;
                        @try {
                            levelVal = [elem valueForKey:@"accessibilityHeadingLevel"];
                        } @catch (NSException *e) {
                            // AXHeadingLevel is not available via KVC on all elements
                        }
                        if (levelVal && [levelVal respondsToSelector:@selector(integerValue)]) {
                            [props addObject:@{@"name": @"level",
                                               @"value": [NSNumber numberWithInteger:[levelVal integerValue]]}];
                        }
                    }
                }

                // Checked state
                if ([elem respondsToSelector:@selector(accessibilityValue)] &&
                    ([ariaRole isEqualToString:@"checkbox"] || [ariaRole isEqualToString:@"radio"] ||
                     [ariaRole isEqualToString:@"switch"])) {
                    id val = [elem accessibilityValue];
                    if ([val isKindOfClass:[NSNumber class]]) {
                        [props addObject:@{@"name": @"checked",
                                           @"value": @([val boolValue])}];
                    }
                }

                // URL for links
                if ([ariaRole isEqualToString:@"link"]) {
                    @try {
                        NSURL *urlVal = nil;
                        if ([elem respondsToSelector:@selector(accessibilityURL)]) {
                            urlVal = [elem accessibilityURL];
                        }
                        if ([urlVal isKindOfClass:[NSURL class]]) {
                            [props addObject:@{@"name": @"url",
                                               @"value": [urlVal absoluteString]}];
                        }
                    } @catch (NSException *e) {}
                }

                // Disabled state
                if ([elem respondsToSelector:@selector(isAccessibilityEnabled)]) {
                    if (![elem isAccessibilityEnabled]) {
                        [props addObject:@{@"name": @"disabled", @"value": @YES}];
                    }
                }

                // Expanded state
                if ([elem respondsToSelector:@selector(isAccessibilityExpanded)]) {
                    @try {
                        BOOL expanded = [elem isAccessibilityExpanded];
                        [props addObject:@{@"name": @"expanded",
                                           @"value": @(expanded)}];
                    } @catch (NSException *e) {}
                }

                // Selected state
                if ([elem respondsToSelector:@selector(isAccessibilitySelected)]) {
                    if ([elem isAccessibilitySelected]) {
                        [props addObject:@{@"name": @"selected", @"value": @YES}];
                    }
                }

                // Required state
                if ([elem respondsToSelector:@selector(isAccessibilityRequired)]) {
                    @try {
                        if ([elem isAccessibilityRequired]) {
                            [props addObject:@{@"name": @"required", @"value": @YES}];
                        }
                    } @catch (NSException *e) {}
                }

                // Focused state
                if ([elem respondsToSelector:@selector(isAccessibilityFocused)]) {
                    if ([elem isAccessibilityFocused]) {
                        [props addObject:@{@"name": @"focused", @"value": @YES}];
                    }
                }

                // Value for input-like elements
                if ([elem respondsToSelector:@selector(accessibilityValue)] &&
                    ([ariaRole isEqualToString:@"textbox"] || [ariaRole isEqualToString:@"combobox"] ||
                     [ariaRole isEqualToString:@"searchbox"] || [ariaRole isEqualToString:@"slider"] ||
                     [ariaRole isEqualToString:@"spinbutton"])) {
                    id val = [elem accessibilityValue];
                    if ([val isKindOfClass:[NSString class]] && [val length] > 0) {
                        [props addObject:@{@"name": @"value", @"value": val}];
                    } else if ([val isKindOfClass:[NSNumber class]]) {
                        [props addObject:@{@"name": @"value",
                                           @"value": [val stringValue]}];
                    }
                }

                // Build node dict
                NSMutableDictionary *node = [NSMutableDictionary dictionary];
                node[@"nodeId"] = nodeId;
                if (parentId) node[@"parentId"] = parentId;
                node[@"role"] = ariaRole;
                if (name.length > 0) node[@"name"] = name;
                if (desc.length > 0) node[@"description"] = desc;
                node[@"properties"] = props;
                node[@"ignored"] = @NO;
                [nodes addObject:node];

                // Recurse into children
                if ([elem respondsToSelector:@selector(accessibilityChildren)]) {
                    NSArray *children = [elem accessibilityChildren];
                    for (id child in children) {
                        void (^strongWalk)(id, NSString*, int) = weakWalkTree;
                        if (strongWalk) strongWalk(child, nodeId, depth + 1);
                    }
                }
            };
            weakWalkTree = walkTree;

            // Start walking from the WKWebView.
            // After WKWebView finishes loading (didFinishNavigation), the
            // NSAccessibility tree may not be populated yet -- the WebContent
            // process needs runloop cycles to bridge its AX tree.  Retry
            // until we see a RootWebArea node (max ~200 ms).
            walkTree(v->webview, nil, 0);
            BOOL hasWebArea = NO;
            for (NSDictionary *n in nodes) {
                if ([@"RootWebArea" isEqualToString:n[@"role"]]) { hasWebArea = YES; break; }
            }
            for (int attempt = 0; attempt < 4 && !hasWebArea; attempt++) {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);
                [nodes removeAllObjects];
                nodeCounter = 0;
                [elemToId removeAllObjects];
                walkTree(v->webview, nil, 0);
                for (NSDictionary *n in nodes) {
                    if ([@"RootWebArea" isEqualToString:n[@"role"]]) { hasWebArea = YES; break; }
                }
            }

            // NSAccessibility doesn't expose the web content tree when
            // WKWebView loads data: URLs (the WebContent process doesn't
            // establish the AX bridge in this case).  Playwright works
            // around this by using the WebKit inspector protocol; since
            // that isn't available here, build the tree from the DOM.
            if (!hasWebArea) {
                __block NSString *jsResult = nil;
                __block BOOL jsDone = NO;
                [v->webview evaluateJavaScript:g_ax_tree_js completionHandler:^(id result, NSError *error) {
                    if (!error && [result isKindOfClass:[NSString class]]) {
                        jsResult = result;
                    }
                    jsDone = YES;
                }];
                for (int w = 0; w < 200 && !jsDone; w++) {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, false);
                }
                if (jsResult) {
                    write_frame_str(req_id, REP_VALUE, jsResult);
                    break;
                }
            }

            // Serialize to JSON
            NSError *jsonErr = nil;
            NSData *jsonData = [NSJSONSerialization dataWithJSONObject:nodes
                                                              options:0
                                                                error:&jsonErr];
            if (jsonErr) {
                write_frame_str(req_id, REP_ERROR, [jsonErr localizedDescription]);
                break;
            }

            NSString *jsonStr = [[NSString alloc] initWithData:jsonData
                                                      encoding:NSUTF8StringEncoding];
            write_frame_str(req_id, REP_VALUE, jsonStr);
            break;
        }

        case OP_ROUTE_REQUEST: {
            // Response from Rust parent for a route request.
            // Payload: u32 strLen + str actionJson
            // Resolve the pending JS replyHandler for this req_id.
            uint32_t off = 0;
            NSString *actionJson = read_str(payload, payload_len, &off);
            NSNumber *key = @(req_id);
            void (^handler)(id, NSString*) = g_pending_routes[key];
            if (handler) {
                [g_pending_routes removeObjectForKey:key];
                // Parse the action JSON and reply to JS
                NSData *jsonData = [actionJson dataUsingEncoding:NSUTF8StringEncoding];
                NSDictionary *action = [NSJSONSerialization JSONObjectWithData:jsonData options:0 error:nil];
                handler(action ?: @{@"action": @"continue"}, nil);
            }
            break;
        }

        case OP_SHUTDOWN:
            _exit(0);

        default: {
            NSString *msg = [NSString stringWithFormat:@"unknown op %d", op];
            write_frame_str(req_id, REP_ERROR, msg);
            break;
        }
    }
    }
}

// ─── Entry point (called from Rust via FFI) ─────────────────────────────────

void fd_webkit_host_main(int fd) __attribute__((noreturn));

void fd_webkit_host_main(int fd) {
    g_fd = fd;

    // Set nonblocking (same as Bun: fcntl O_NONBLOCK)
    int fl = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, fl | O_NONBLOCK);

    // Initialize AppKit
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];

    // Disable App Nap (same as Bun: beginActivityWithOptions)
    [[NSProcessInfo processInfo]
        beginActivityWithOptions:(NSActivityUserInitiatedAllowingIdleSystemSleep | NSActivityLatencyCritical)
        reason:@"ferridriver webkit host"];

    // Initialize state
    g_views = [NSMutableDictionary new];
    g_nav_delegate = [[FDNavDelegate alloc] init];
    g_rx = [NSMutableData dataWithCapacity:65536];
    g_write_queue = [NSMutableData new];

    // CFFileDescriptor wrapping the socket fd — single callback handles
    // both read and write (same as Bun's cfCallback)
    g_cffd = CFFileDescriptorCreate(NULL, fd, true, cf_callback, NULL);
    CFRunLoopSourceRef src = CFFileDescriptorCreateRunLoopSource(NULL, g_cffd, 0);
    CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopDefaultMode);
    CFRelease(src);
    CFFileDescriptorEnableCallBacks(g_cffd, kCFFileDescriptorReadCallBack);

    // CFRunLoopRun — blocks as main loop. Properly integrates with
    // CVDisplayLink, AppKit events, WKWebView rendering callbacks.
    CFRunLoopRun();

    _exit(0);
}
