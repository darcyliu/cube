#import <AppKit/AppKit.h>
#import <Metal/Metal.h>
#import <MetalKit/MetalKit.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <math.h>

extern bool vmm_native_finished(void);
extern void vmm_native_close(void);
extern void vmm_native_release_input(void);
extern bool vmm_native_geometry(uint32_t *width, uint32_t *height);
extern bool vmm_native_copy_frame(void *pixels, size_t length);
extern void vmm_native_key(uint16_t key, bool down);
extern void vmm_native_pointer(uint16_t x, uint16_t y, uint8_t buttons);

static NSString *const shaderSource = @"#include <metal_stdlib>\n"
    "using namespace metal;\n"
    "struct Vertex { float4 position [[position]]; float2 uv; };\n"
    "vertex Vertex vertex_main(uint id [[vertex_id]], constant float4 &placement [[buffer(0)]]) {\n"
    "  float2 p[4] = { {-1,-1}, {1,-1}, {-1,1}, {1,1} };\n"
    "  float2 t[4] = { {0,1}, {1,1}, {0,0}, {1,0} };\n"
    "  Vertex v; v.position = float4(p[id] * placement.xy + placement.zw, 0, 1); v.uv = t[id]; return v;\n"
    "}\n"
    "fragment float4 fragment_main(Vertex v [[stage_in]], texture2d<float> image [[texture(0)]]) {\n"
    "  constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::nearest);\n"
    "  return image.sample(s, v.uv);\n"
    "}\n";

// Work in drawable pixels, not AppKit points. Integer zoom on Retina keeps each
// guest pixel aligned to a whole number of physical screen pixels.
static NSRect guestRect(CGSize drawable, uint32_t width, uint32_t height) {
    CGFloat fit = MIN(drawable.width / width, drawable.height / height);
    CGFloat zoom = fit >= 1 ? floor(fit) : fit;
    CGFloat w = MAX(1, floor(width * zoom));
    CGFloat h = MAX(1, floor(height * zoom));
    return NSMakeRect(floor((drawable.width - w) / 2),
                      floor((drawable.height - h) / 2), w, h);
}

// A timer can call stop: while AppKit is waiting for its next event. Post a
// harmless event too, so NSApplication.run actually returns when the VM ends.
static void stopApp(void) {
    [NSApp stop:nil];
    NSEvent *wake = [NSEvent otherEventWithType:NSEventTypeApplicationDefined
        location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0
        context:nil subtype:0 data1:0 data2:0];
    [NSApp postEvent:wake atStart:YES];
}

@interface VmmView : MTKView <MTKViewDelegate, NSWindowDelegate> {
    id<MTLRenderPipelineState> _pipeline;
    id<MTLCommandQueue> _queue;
    id<MTLTexture> _frame;
    void *_pixels;
    size_t _pixelLength;
    uint32_t _guestWidth;
    uint32_t _guestHeight;
    uint8_t _buttons;
    bool _modifierKeys[128];
    bool _reportedFrame;
    bool _failed;
    CGFloat _scrollRemainder;
}
- (BOOL)failed;
@end

@implementation VmmView

- (instancetype)initWithFrame:(NSRect)frame device:(id<MTLDevice>)device {
    self = [super initWithFrame:frame device:device];
    if (!self) return nil;
    self.delegate = self;
    self.colorPixelFormat = MTLPixelFormatBGRA8Unorm;
    self.autoResizeDrawable = YES;
    self.clearColor = MTLClearColorMake(0, 0, 0, 1);
    self.preferredFramesPerSecond = 30;
    self.paused = NO;
    _queue = [device newCommandQueue];
    NSError *error = nil;
    id<MTLLibrary> library = [device newLibraryWithSource:shaderSource options:nil error:&error];
    if (!library) {
        NSLog(@"VMM Metal shader: %@", error);
        return nil;
    }
    MTLRenderPipelineDescriptor *desc = [MTLRenderPipelineDescriptor new];
    desc.vertexFunction = [library newFunctionWithName:@"vertex_main"];
    desc.fragmentFunction = [library newFunctionWithName:@"fragment_main"];
    desc.colorAttachments[0].pixelFormat = self.colorPixelFormat;
    _pipeline = [device newRenderPipelineStateWithDescriptor:desc error:&error];
    if (!_pipeline) {
        NSLog(@"VMM Metal pipeline: %@", error);
        return nil;
    }
    self.enableSetNeedsDisplay = NO;
    [self addTrackingArea:[[NSTrackingArea alloc]
        initWithRect:NSZeroRect
        options:NSTrackingMouseMoved | NSTrackingActiveInKeyWindow | NSTrackingInVisibleRect
        owner:self userInfo:nil]];
    return self;
}

- (void)dealloc {
    free(_pixels);
}

- (BOOL)acceptsFirstResponder { return YES; }
- (BOOL)failed { return _failed; }

- (void)mtkView:(MTKView *)view drawableSizeWillChange:(CGSize)size {
    (void)view;
    (void)size;
}

- (void)drawInMTKView:(MTKView *)view {
    (void)view;
    if (vmm_native_finished()) {
        stopApp();
        return;
    }
    uint32_t width = 0, height = 0;
    if (!vmm_native_geometry(&width, &height)) return;
    if (width != _guestWidth || height != _guestHeight) {
        _guestWidth = width;
        _guestHeight = height;
        free(_pixels);
        _pixelLength = (size_t)width * height * 4;
        _pixels = malloc(_pixelLength);
        MTLTextureDescriptor *desc = [MTLTextureDescriptor
            texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
            width:width height:height mipmapped:NO];
        desc.usage = MTLTextureUsageShaderRead;
        desc.storageMode = MTLStorageModeShared;
        _frame = [self.device newTextureWithDescriptor:desc];
        if (!_pixels || !_frame) {
            NSLog(@"VMM cannot allocate native framebuffer");
            _failed = true;
            stopApp();
            return;
        }
    }
    if (!vmm_native_copy_frame(_pixels, _pixelLength)) return;
    [_frame replaceRegion:MTLRegionMake2D(0, 0, width, height)
            mipmapLevel:0 withBytes:_pixels bytesPerRow:width * 4];
    CGSize drawableSize = self.drawableSize;
    if (drawableSize.width <= 0 || drawableSize.height <= 0) return;
    NSRect rect = guestRect(drawableSize, width, height);
    MTLRenderPassDescriptor *pass = self.currentRenderPassDescriptor;
    id<CAMetalDrawable> drawable = self.currentDrawable;
    if (!pass || !drawable) return;
    id<MTLCommandBuffer> command = [_queue commandBuffer];
    id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
    [encoder setRenderPipelineState:_pipeline];
    float placement[4] = {
        (float)(rect.size.width / drawableSize.width),
        (float)(rect.size.height / drawableSize.height),
        (float)((2 * rect.origin.x + rect.size.width) / drawableSize.width - 1),
        (float)((2 * rect.origin.y + rect.size.height) / drawableSize.height - 1),
    };
    [encoder setVertexBytes:placement length:sizeof(placement) atIndex:0];
    [encoder setFragmentTexture:_frame atIndex:0];
    [encoder drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4];
    [encoder endEncoding];
    [command presentDrawable:drawable];
    [command commit];
    if (!_reportedFrame) {
        fprintf(stderr, "[Native] First Metal frame: guest %ux%u, drawable %.0fx%.0f, image %.0fx%.0f\n",
                width, height, drawableSize.width, drawableSize.height,
                rect.size.width, rect.size.height);
        _reportedFrame = true;
    }
}

- (void)sendPointer:(NSEvent *)event extra:(uint8_t)extra {
    if (!_guestWidth || !_guestHeight) return;
    NSPoint p = [self convertPoint:event.locationInWindow fromView:nil];
    NSPoint backing = [self convertPointToBacking:p];
    CGSize drawableSize = self.drawableSize;
    if (drawableSize.width <= 0 || drawableSize.height <= 0) return;
    NSRect rect = guestRect(drawableSize, _guestWidth, _guestHeight);
    double x = (backing.x - rect.origin.x) * _guestWidth / rect.size.width;
    double y = _guestHeight - 1 -
               (backing.y - rect.origin.y) * _guestHeight / rect.size.height;
    uint16_t gx = (uint16_t)MAX(0, MIN(_guestWidth - 1, x));
    uint16_t gy = (uint16_t)MAX(0, MIN(_guestHeight - 1, y));
    vmm_native_pointer(gx, gy, _buttons | extra);
}

- (void)mouseMoved:(NSEvent *)event { [self sendPointer:event extra:0]; }
- (void)mouseDragged:(NSEvent *)event { [self sendPointer:event extra:0]; }
- (void)rightMouseDragged:(NSEvent *)event { [self sendPointer:event extra:0]; }
- (void)otherMouseDragged:(NSEvent *)event { [self sendPointer:event extra:0]; }
- (void)mouseDown:(NSEvent *)event {
    [self.window makeFirstResponder:self]; _buttons |= 1; [self sendPointer:event extra:0];
}
- (void)mouseUp:(NSEvent *)event { _buttons &= ~1; [self sendPointer:event extra:0]; }
- (void)rightMouseDown:(NSEvent *)event { _buttons |= 4; [self sendPointer:event extra:0]; }
- (void)rightMouseUp:(NSEvent *)event { _buttons &= ~4; [self sendPointer:event extra:0]; }
- (void)otherMouseDown:(NSEvent *)event { _buttons |= 2; [self sendPointer:event extra:0]; }
- (void)otherMouseUp:(NSEvent *)event { _buttons &= ~2; [self sendPointer:event extra:0]; }
- (void)scrollWheel:(NSEvent *)event {
    _scrollRemainder += event.scrollingDeltaY;
    int steps = 0;
    while (_scrollRemainder >= 1 && steps++ < 8) {
        [self sendPointer:event extra:8]; _scrollRemainder -= 1;
    }
    steps = 0;
    while (_scrollRemainder <= -1 && steps++ < 8) {
        [self sendPointer:event extra:16]; _scrollRemainder += 1;
    }
    _scrollRemainder = MAX(-8, MIN(8, _scrollRemainder));
    [self sendPointer:event extra:0];
}
- (void)keyDown:(NSEvent *)event { vmm_native_key(event.keyCode, true); }
- (void)keyUp:(NSEvent *)event { vmm_native_key(event.keyCode, false); }
- (void)flagsChanged:(NSEvent *)event {
    NSUInteger key = event.keyCode;
    if (key == 57) {
        vmm_native_key(57, true);
        vmm_native_key(57, false);
        return;
    }
    if (key < 128) {
        _modifierKeys[key] = !_modifierKeys[key];
        vmm_native_key((uint16_t)key, _modifierKeys[key]);
    }
}
- (void)windowDidResignKey:(NSNotification *)notification {
    (void)notification;
    memset(_modifierKeys, 0, sizeof(_modifierKeys));
    _buttons = 0;
    vmm_native_release_input();
}
- (void)windowWillClose:(NSNotification *)notification {
    (void)notification;
    vmm_native_close();
    stopApp();
}
- (void)tick:(NSTimer *)timer {
    (void)timer;
    if (vmm_native_finished()) stopApp();
}
@end

bool vmm_cocoa_run(uint32_t width, uint32_t height) {
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) {
            NSLog(@"VMM requires a Metal-capable display");
            vmm_native_close();
            return false;
        }
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyRegular];
        NSRect frame = NSMakeRect(0, 0, MIN(width, 1280), MIN(height, 800));
        NSWindow *window = [[NSWindow alloc]
            initWithContentRect:frame
            styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                      NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
            backing:NSBackingStoreBuffered defer:NO];
        if (!window) {
            NSLog(@"VMM cannot create the Cocoa window");
            vmm_native_close();
            return false;
        }
        VmmView *view = [[VmmView alloc] initWithFrame:frame device:device];
        if (!view) {
            vmm_native_close();
            return false;
        }
        window.contentView = view;
        window.delegate = view;
        window.title = @"VMM — FreeBSD";
        [window center];
        [window makeKeyAndOrderFront:nil];
        [window makeFirstResponder:view];
        fprintf(stderr, "[Native] Cocoa window ready (%ux%u)\n", width, height);
        [NSTimer scheduledTimerWithTimeInterval:0.1 target:view
            selector:@selector(tick:) userInfo:nil repeats:YES];
        [app activateIgnoringOtherApps:YES];
        [app run];
        vmm_native_close();
        return ![view failed];
    }
}
