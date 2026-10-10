/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIWindow`.
//!
//! Useful resources:
//! - [Technical Q&A QA1588: Automatic orientation support for iPhone and iPad apps](https://developer.apple.com/library/archive/qa/qa1588/_index.html)
//! - [Technical Q&A QA1688: Why won't my UIViewController rotate with the device?](https://developer.apple.com/library/archive/qa/qa1688/_index.html)

use super::UIViewHostObject;
use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::core_graphics::cg_affine_transform::{
    CGAffineTransform, CGAffineTransformIdentity,
};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::{ns_string, NSUInteger};
use crate::frameworks::uikit::ui_application::{
    UIInterfaceOrientationLandscapeLeft, UIInterfaceOrientationLandscapeRight,
    UIInterfaceOrientationPortraitUpsideDown,
};
use crate::frameworks::uikit::ui_device::{
    UIDeviceOrientationLandscapeLeft, UIDeviceOrientationLandscapeRight,
    UIDeviceOrientationPortraitUpsideDown,
};
use crate::objc::{
    id, msg, msg_class, msg_super, nil, objc_classes, release, retain, ClassExports,
};
use crate::Environment;
use std::collections::HashMap;

#[derive(Default)]
pub struct State {
    /// List of visible windows for internal purposes. Non-retaining!
    ///
    /// This is public because Core Animation also uses it.
    pub windows: Vec<id>,
    /// The most recent window which received `makeKeyAndVisible` message.
    /// Non-retaining!
    pub key_window: Option<id>,
    /// Per-window `rootViewController` association (iOS 4+).
    ///
    /// `UIWindow` itself derives from `UIView` whose host object is shared
    /// with every plain view; we therefore keep the root-VC association out
    /// of the per-view struct and indexed by window pointer instead. The VC
    /// is retained while the window holds it, mirroring Apple's documented
    /// strong-property semantics.
    pub root_view_controllers: HashMap<id, id>,
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIWindow: UIView

// TODO: more?

// UIWindow levels are only relevant when multiple windows overlap. The
// compositor already draws windows in insertion order, so retain the value
// for API compatibility without changing the single-screen ordering.
- (())setWindowLevel:(f32)_level { }
- (f32)windowLevel { 0.0 }

- (id)initWithFrame:(CGRect)frame {
    let this = msg_super![env; this initWithFrame:frame];
    // Undocumented: windows seem to be hidden by default on iOS, unlike views.
    // Super call to bypass the overriden setter on this class, which would post
    // a notification.
    () = msg_super![env; this setHidden:true];

    let list = &mut env.framework_state.uikit.ui_view.ui_window.windows;
    list.push(this);
    log_dbg!(
        "New window: {:?}. New list of all windows: {:?}",
        this,
        list,
    );

    this
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    let this = msg_super![env; this initWithCoder:coder];
    // Undocumented: windows seem to be hidden by default on iOS, unlike views.
    // Super call to bypass the overriden setter on this class, which would post
    // a notification.
    () = msg_super![env; this setHidden:true];

    // Real iOS forces UIWindow.frame to match UIScreen.bounds regardless of
    // whatever frame was encoded in the NIB. Interface Builder by default
    // uses a 320x460 canvas (status bar visible) for iPhone XIBs, so a
    // window loaded from a NIB will normally come out at 320x460 and miss
    // the bottom 20px of the screen. Apps that hide the status bar via
    // Info.plist (e.g. Minecraft PE 0.6.x) then end up with their EAGLView
    // sized to the truncated window, producing a visible offset/shift.
    //
    // Mirror Apple's behaviour by re-setting the frame to the full screen
    // bounds after the super decoder runs.
    let screen: id = msg_class![env; UIScreen mainScreen];
    let screen_bounds: CGRect = msg![env; screen bounds];
    let current_bounds: CGRect = msg![env; this bounds];
    if current_bounds.size != screen_bounds.size {
        log_dbg!(
            "UIWindow {:?}: overriding NIB-encoded size {:?} with UIScreen.bounds size {:?}",
            this,
            current_bounds.size,
            screen_bounds.size,
        );
        () = msg![env; this setFrame:screen_bounds];
    }

    let list = &mut env.framework_state.uikit.ui_view.ui_window.windows;
    list.push(this);
    log_dbg!(
        "New window: {:?}. New list of all windows: {:?}",
        this,
        list,
    );

    this
}

- (())dealloc {
    if let Some(key_window) = env.framework_state.uikit.ui_view.ui_window.key_window {
        if key_window == this {
            env.framework_state.uikit.ui_view.ui_window.key_window = None;
        }
    }
    // Release the strong `rootViewController` reference, if any.
    if let Some(root_vc) = env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .root_view_controllers
        .remove(&this)
    {
        release(env, root_vc);
    }
    let list = &mut env.framework_state.uikit.ui_view.ui_window.windows;
    // A window may be deallocated without ever being registered (e.g.
    // `[[UIWindow alloc] init]`, which skips both designated initializers).
    // Never panic on guest-driven paths.
    if let Some(idx) = list.iter().position(|&w| w == this) {
        list.remove(idx);
    }
    log_dbg!(
        "Deallocating window {:?}. New list of all windows: {:?}",
        this,
        list,
    );
    msg_super![env; this dealloc]
}

- (())layoutIfNeeded {
    log_dbg!("[(UIWindow*){:?} layoutIfNeeded]", this);
    // Честная реализация: немедленно форсируем пересчет layout'а,
    // отправляя сообщение layoutSubviews самому себе (наследуется от UIView)
    () = msg![env; this layoutSubviews];
}

// Permissive hit-testing for the application window.
//
// On real iOS, UIWindow's frame matches UIScreen.bounds and touches always
// land inside it, so the standard UIView hitTest implementation is fine.
// In touchHLE, however, the host window can be larger than the iPhone's
// virtual screen (Android phones, large desktop windows). Even with the
// touch-coordinate clamp in `transform_input_coords`, edge cases such as
// the rootViewController's view being rotated for landscape orientation
// can leave individual modal/overlay subviews positioned in such a way
// that a touch landing on them produces a window-coordinate point just
// outside of UIWindow's own bounds (off-by-one at the bottom row, status
// bar overlap, etc.). The default UIView.hitTest then early-exits via
// pointInside, returning nil for the entire touch — which is what
// produced the `SUPER HACK: Forcing rejected touch ...` log spam and
// stopped the in-game chat / Create World text fields from ever becoming
// first responder.
//
// Override hitTest to ALWAYS recurse into subviews. If any subview claims
// the touch we return it; otherwise we fall back to the window itself so
// touch dispatch is never lost. This matches Apple's documented intent:
// "Windows don't actively participate in event handling. They merely
// pass events on to their subviews."
- (id)hitTest:(CGPoint)point withEvent:(id)event {
    let subviews = env.objc.borrow::<super::UIViewHostObject>(this).subviews.clone();
    for subview in subviews.into_iter().rev() {
        let hidden: bool = msg![env; subview isHidden];
        let alpha: crate::frameworks::core_graphics::CGFloat = msg![env; subview alpha];
        let interactible: bool = msg![env; subview isUserInteractionEnabled];
        if hidden || alpha < 0.01 || !interactible { continue; }
        let sub_point: CGPoint = msg![env; subview convertPoint:point fromView:this];
        let hit: id = msg![env; subview hitTest:sub_point withEvent:event];
        if hit != nil { return hit; }
    }
    this
}

- (())setHidden:(bool)is_hidden {
    () = msg_super![env; this setHidden:is_hidden];

    let notif_name = ns_string::get_static_str(
        env,
        if is_hidden {
            UIWindowDidBecomeHiddenNotification
        } else {
            UIWindowDidBecomeVisibleNotification
        },
    );
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    () = msg![env; center postNotificationName:notif_name object:this userInfo:nil];

    log_dbg!("[(UIWindow*){:?} setHidden:{:?}]", this, is_hidden);
}

- (())makeKeyWindow {
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];

    if let Some(previous) = env.framework_state.uikit.ui_view.ui_window.key_window {
        if previous != this {
            let notif_name =
                ns_string::get_static_str(env, UIWindowDidResignKeyNotification);
            () = msg![env; center postNotificationName:notif_name object:previous userInfo:nil];
        }
    }

    env.framework_state.uikit.ui_view.ui_window.key_window = Some(this);

    let notif_name = ns_string::get_static_str(env, UIWindowDidBecomeKeyNotification);
    () = msg![env; center postNotificationName:notif_name object:this userInfo:nil];
}

- (bool)isKeyWindow {
    env.framework_state.uikit.ui_view.ui_window.key_window == Some(this)
}

- (())makeKeyAndVisible {
    // MakeKeyVisibleFix (from XaViewDnK fork): some engines (e.g. Asphalt 8's
    // Jet) create the window with a zero frame and only size it via
    // `makeKeyAndVisible`. Without this, the window stays 0x0 and every
    // layer composite is empty => black screen.
    let screen: id = msg_class![env; UIScreen mainScreen];
    let bounds: CGRect = msg![env; screen bounds];
    let frame: CGRect = msg![env; this frame];
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        log!("Fixing empty window frame to {:?}", bounds);
        () = msg![env; this setFrame:bounds];
    }

    // Bump the window to the top of the touch list (was a FIXME upstream).
    let list = &mut env.framework_state.uikit.ui_view.ui_window.windows;
    if let Some(idx) = list.iter().position(|&w| w == this) {
        let w = list.remove(idx);
        list.push(w);
    }

    () = msg![env; this makeKeyWindow];
    () = msg![env; this setHidden:false];
}

// Legacy iOS 2/3 pattern: [window setContentView:someView]
// Semantically equivalent to addSubview: for UIWindow.
- (())setContentView:(id)view {
    log_dbg!("[(UIWindow*){:?} setContentView:{:?}]", this, view);
    () = msg![env; this addSubview:view];
}

// Returns the first subview as the content view (legacy behaviour).
- (id)contentView {
    let subviews = &env.objc.borrow::<UIViewHostObject>(this).subviews;
    subviews.first().copied().unwrap_or(nil)
}

// Support for rootViewController (iOS 4+)
// Per Apple's [UIWindow Reference](https://developer.apple.com/documentation/uikit/uiwindow/1621581-rootviewcontroller):
// `rootViewController` is a strong reference. Setting a new root view
// controller installs its `-view` as a full-bounds subview of the window,
// optionally replacing the previously installed root view. We mirror that
// contract exactly: retain the new VC, release the old one, swap the
// associated view, and notify the existing appearance lifecycle through
// `-addSubview:`.
- (())setRootViewController:(id)view_controller {
    log_dbg!("[(UIWindow*){:?} setRootViewController:{:?}]", this, view_controller);

    let previous = env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .root_view_controllers
        .get(&this)
        .copied()
        .unwrap_or(nil);

    if previous == view_controller {
        return;
    }

    // Retain BEFORE releasing the old one, so that if the new VC was the
    // last strong holder of the old VC we don't temporarily drop it.
    if view_controller != nil {
        retain(env, view_controller);
    }

    if previous != nil {
        let previous_view: id = msg![env; previous view];
        if previous_view != nil {
            () = msg![env; previous_view removeFromSuperview];
        }
        release(env, previous);
    }

    if view_controller != nil {
        let view: id = msg![env; view_controller view];
        // Apple's docs: the root view controller's view is resized to fill
        // the window's bounds. Match the window bounds before adding so
        // -layoutSubviews observes the right geometry.
        let bounds: CGRect = msg![env; this bounds];
        () = msg![env; view setFrame:bounds];
        () = msg![env; this addSubview:view];
        env.framework_state
            .uikit
            .ui_view
            .ui_window
            .root_view_controllers
            .insert(this, view_controller);
    } else {
        env.framework_state
            .uikit
            .ui_view
            .ui_window
            .root_view_controllers
            .remove(&this);
    }
}

- (id)rootViewController {
    env.framework_state
        .uikit
        .ui_view
        .ui_window
        .root_view_controllers
        .get(&this)
        .copied()
        .unwrap_or(nil)
}

// UIResponder implementation
// From the Apple UIView docs regarding [UIResponder nextResponder]:
// "UIWindow returns the application object."
- (id)nextResponder {
    msg_class![env; UIApplication sharedApplication]
}

- (())addSubview:(id)view {
    log_dbg!("[(UIWindow*){:?} addSubview:{:?}] => ()", this, view);

    if view == nil || env.objc.borrow::<UIViewHostObject>(view).view_controller == nil {
        () = msg_super![env; this addSubview:view];
        return;
    }

    // Below we treat a special case of adding a view controller's view
    // to a window, in order to generate the appropriate display-related
    // notifications. Apple's UIViewController contract:
    //   - viewWillAppear: is sent before the view is added to the
    //     view hierarchy AND will become visible.
    //   - viewDidAppear:  is sent after that.
    // When the view is already a subview, [UIView addSubview:] still
    // moves it to the top of the subviews array (Apple docs:
    // "Calls to this method with a view that is already a subview
    //  result in the view being moved to the end of the subview list.")
    // After moving to the top there are no obstructing siblings, so the
    // appearance lifecycle is fired iff the view is not explicitly
    // hidden via -setHidden:YES. The previous TODO existed because
    // touchHLE had no way to detect this case; we now defer to
    // -isHidden, which mirrors UIKit's actual visibility test.
    let was_subview = env
        .objc
        .borrow::<UIViewHostObject>(this)
        .subviews
        .contains(&view);
    let view_hidden: bool = msg![env; view isHidden];
    let should_fire_appearance = !view_hidden;
    if was_subview {
        log_dbg!(
            "[(UIWindow*){:?} addSubview:{:?}]: re-adding existing subview; \
             moving to top, appearance lifecycle will{} fire (hidden={}).",
            this,
            view,
            if should_fire_appearance { "" } else { " not" },
            view_hidden
        );
    }

    let vc = env.objc.borrow::<UIViewHostObject>(view).view_controller;
    if should_fire_appearance {
        () = msg![env; vc viewWillAppear:false];
    }
    () = msg_super![env; this addSubview:view];
    if should_fire_appearance {
        () = msg![env; vc viewDidAppear:false];
    }

    // Support auto-rotation. This is currently only for apps that request a
    // non-portrait interface orientation via Info.plist, as we do not yet
    // support changes of orientation caused by device rotation (TODO).
    // FIXME: It's unclear when and where this auto-rotation is supposed to
    //        happen. It must have something to do with mounting the view
    //        controller to a window, so we do it here. QA1688 (see top of file)
    //        mentions a breaking behaviour change in iOS 6 that makes
    //        auto-rotation rely on rootViewController (a property only found in
    //        iOS 6), so the current implementation is specific to iOS <= 5.
    // FIXME: Are we supposed to notify the view somehow of the rotation?
    // FIXME: What do we do if shouldAutorotateToInterfaceOrientation:
    //        returns false? The status bar has already been rotated…
    // FIXME: The device orientation stored on env.window can come from one of
    //        three places (user/default options, setStatusBarOrientation: etc,
    //        Info.plist UIInterfaceOrientation etc). It's not clear if these
    //        are really equivalent and should all trigger autorotation.
    // `env.window` is `None` in headless mode; skip autorotation instead of
    // unwrapping (which would panic the host).
    let rotation = env.window.as_ref().map(|window| window.current_rotation());
    if let Some(orientation) = rotation.and_then(|rotation| match rotation {
        crate::window::DeviceOrientation::LandscapeLeft => Some(UIDeviceOrientationLandscapeLeft),
        crate::window::DeviceOrientation::LandscapeRight => Some(UIDeviceOrientationLandscapeRight),
        crate::window::DeviceOrientation::PortraitUpsideDown => {
            Some(UIDeviceOrientationPortraitUpsideDown)
        }
        // Portrait is the default so we don't do anything here.
        crate::window::DeviceOrientation::Portrait => None,
    }) {
        // (UIInterfaceOrientation and UIDeviceOrientation are compatible enums,
        //  here we use whichever is clearer contextually.)
        let should = msg![env; vc shouldAutorotateToInterfaceOrientation:orientation];
        log_dbg!("[{:?} shouldAutorotateToInterfaceOrientation:{:?}] => {:?}", vc, orientation, should);
        if should {
            log_dbg!("App requested autorotation; applying orientation transform to view {:?}.", view);
            let transform = match orientation {
                UIInterfaceOrientationLandscapeLeft => CGAffineTransform::make_rotation(-std::f32::consts::FRAC_PI_2),
                UIInterfaceOrientationLandscapeRight => CGAffineTransform::make_rotation(std::f32::consts::FRAC_PI_2),
                UIInterfaceOrientationPortraitUpsideDown => CGAffineTransform::make_rotation(std::f32::consts::PI),
                other => {
                    // UIInterfaceOrientation has Portrait/PortraitUpsideDown/
                    // LandscapeLeft/LandscapeRight; the first two are filtered
                    // out earlier (Portrait => None and PortraitUpsideDown
                    // isn't reachable from window::DeviceOrientation today).
                    // Fall back to the identity transform so an unexpected
                    // orientation can't take down the host.
                    log!(
                        "Warning: UIWindow autorotation: unsupported interface orientation {}; using identity transform.",
                        other
                    );
                    CGAffineTransformIdentity
                }
            };

            let window_frame: CGRect = msg![env; this frame];
            log_dbg!("Window frame: {window_frame:?}");
            let old_root_bounds: CGRect = msg![env; view bounds];
            let view_frame: CGRect = msg![env; view frame];
            log_dbg!("Old view frame: {view_frame:?}");

            () = msg![env; view setTransform:transform];

            // Re-apply the view's old frame to compensate for the rotation
            // effectively offseting its center position and changing the size.
            // FIXME: I have no idea if this is how this should be solved, but
            //        it works for DMC4 Refrain at least.

            let view_frame: CGRect = msg![env; view frame];
            log_dbg!("Old view frame after transform: {view_frame:?}");

            () = msg![env; view setFrame:window_frame];

            let view_frame: CGRect = msg![env; view frame];
            log_dbg!("New view frame after re-applying old view frame: {view_frame:?}");

            if crate::wants_landscape_root_view_resize(env.bundle.bundle_identifier()) {
                propagate_root_view_rotation(env, vc, view, old_root_bounds.size);
            }
        }
    }
}

- (CGPoint)convertPoint:(CGPoint)point
             fromWindow:(id)other { // UIWindow*
    let this_layer: id = msg![env; this layer];
    // Resolves to nil if other is nil.
    let other_layer: id = msg![env; other layer];
    msg![env; this_layer convertPoint:point fromLayer:other_layer]
}
- (CGPoint)convertPoint:(CGPoint)point
               toWindow:(id)other { // UIWindow*
    let this_layer: id = msg![env; this layer];
    // Resolves to nil if other is nil.
    let other_layer: id = msg![env; other layer];
    msg![env; this_layer convertPoint:point toLayer:other_layer]
}

// Apple's
// <https://developer.apple.com/documentation/uikit/uiwindow/1621597-screen>:
// `screen` returns the `UIScreen` the window is currently displayed on,
// and the documented setter (`setScreen:`) is used by apps that wish to
// move a window to an external display. We emulate a single main screen
// only, so the getter always returns `+[UIScreen mainScreen]` and the
// setter is a no-op that just acknowledges the call to keep the
// "doesNotRecognizeSelector" warning from firing on apps that
// unconditionally call `window.screen = UIScreen.mainScreen` during
// `application:didFinishLaunchingWithOptions:`.
- (id)screen {
    msg_class![env; UIScreen mainScreen]
}
- (())setScreen:(id)_screen {
    log_dbg!(
        "[UIWindow setScreen:] ignored; touchHLE only emulates the main screen."
    );
}

@end

};

// MARK: - Landscape root view resizing
// (see crate::wants_landscape_root_view_resize)

const UI_VIEW_AUTORESIZING_FLEXIBLE_LEFT_MARGIN: NSUInteger = 1 << 0;
const UI_VIEW_AUTORESIZING_FLEXIBLE_WIDTH: NSUInteger = 1 << 1;
const UI_VIEW_AUTORESIZING_FLEXIBLE_RIGHT_MARGIN: NSUInteger = 1 << 2;
const UI_VIEW_AUTORESIZING_FLEXIBLE_TOP_MARGIN: NSUInteger = 1 << 3;
const UI_VIEW_AUTORESIZING_FLEXIBLE_HEIGHT: NSUInteger = 1 << 4;
const UI_VIEW_AUTORESIZING_FLEXIBLE_BOTTOM_MARGIN: NSUInteger = 1 << 5;

/// Bound on how deep [resize_subviews_for_rotation] descends, so a
/// pathological (or cyclic, through a guest bug) view tree can't hang us.
const MAX_ROTATION_RESIZE_DEPTH: u32 = 16;

/// Distribute a change in the superview's extent along one axis between the
/// flexible parts (leading margin, size, trailing margin) of a subview, the
/// way UIKit's autoresizing does: proportionally to the current length of
/// each flexible part, or evenly when they are all zero.
///
/// Returns the new `(origin, size)` along that axis.
fn autoresize_axis(
    origin: CGFloat,
    size: CGFloat,
    old_extent: CGFloat,
    new_extent: CGFloat,
    flexible_leading: bool,
    flexible_size: bool,
    flexible_trailing: bool,
) -> (CGFloat, CGFloat) {
    let delta = new_extent - old_extent;
    let flexible_count =
        flexible_leading as u32 + flexible_size as u32 + flexible_trailing as u32;
    if delta == 0.0 || flexible_count == 0 {
        return (origin, size);
    }

    let leading = origin;
    let trailing = old_extent - origin - size;
    let flexible_total = if flexible_leading { leading.max(0.0) } else { 0.0 }
        + if flexible_size { size.max(0.0) } else { 0.0 }
        + if flexible_trailing { trailing.max(0.0) } else { 0.0 };

    let share = |part: CGFloat, flexible: bool| -> CGFloat {
        if !flexible {
            0.0
        } else if flexible_total > 0.0 {
            delta * part.max(0.0) / flexible_total
        } else {
            delta / flexible_count as CGFloat
        }
    };

    let new_origin = origin + share(leading, flexible_leading);
    let new_size = (size + share(size, flexible_size)).max(0.0);
    (new_origin, new_size)
}

/// UIKit's autoresizing of one subview when its superview's bounds size
/// changes from `old_parent` to `new_parent`.
fn autoresized_frame(
    frame: CGRect,
    mask: NSUInteger,
    old_parent: CGSize,
    new_parent: CGSize,
) -> CGRect {
    let (x, width) = autoresize_axis(
        frame.origin.x,
        frame.size.width,
        old_parent.width,
        new_parent.width,
        mask & UI_VIEW_AUTORESIZING_FLEXIBLE_LEFT_MARGIN != 0,
        mask & UI_VIEW_AUTORESIZING_FLEXIBLE_WIDTH != 0,
        mask & UI_VIEW_AUTORESIZING_FLEXIBLE_RIGHT_MARGIN != 0,
    );
    let (y, height) = autoresize_axis(
        frame.origin.y,
        frame.size.height,
        old_parent.height,
        new_parent.height,
        mask & UI_VIEW_AUTORESIZING_FLEXIBLE_TOP_MARGIN != 0,
        mask & UI_VIEW_AUTORESIZING_FLEXIBLE_HEIGHT != 0,
        mask & UI_VIEW_AUTORESIZING_FLEXIBLE_BOTTOM_MARGIN != 0,
    );
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

fn nearly(a: CGFloat, b: CGFloat) -> bool {
    (a - b).abs() < 0.5
}

/// Whether `frame` exactly covers a superview whose bounds size is `parent`.
fn frame_covers(frame: CGRect, parent: CGSize) -> bool {
    nearly(frame.origin.x, 0.0)
        && nearly(frame.origin.y, 0.0)
        && nearly(frame.size.width, parent.width)
        && nearly(frame.size.height, parent.height)
}

fn view_has_eagl_layer(env: &mut Environment, view: id) -> bool {
    let layer: id = msg![env; view layer];
    if layer == nil {
        return false;
    }
    let eagl_layer_class: id = msg_class![env; CAEAGLLayer class];
    msg![env; layer isKindOfClass:eagl_layer_class]
}

/// Resize the subviews of `view` after its bounds size changed from
/// `old_size` to `new_size`, following their `autoresizingMask`. This is what
/// UIKit does inside `-[UIView setBounds:]`/`-setFrame:`; touchHLE's
/// setters don't, so without this the subviews of an autorotated root view
/// keep their portrait frames.
///
/// A `CAEAGLLayer`-backed subview that exactly covered the old bounds is
/// treated as full-size even without a mask: its drawable size decides the
/// shape of everything the game renders, and on a device it would be
/// landscape by the time the game first sizes its renderbuffer.
///
/// Every view whose size changed is appended to `resized`.
fn resize_subviews_for_rotation(
    env: &mut Environment,
    view: id,
    old_size: CGSize,
    new_size: CGSize,
    depth: u32,
    resized: &mut Vec<id>,
) {
    if depth > MAX_ROTATION_RESIZE_DEPTH {
        return;
    }
    if nearly(old_size.width, new_size.width) && nearly(old_size.height, new_size.height) {
        return;
    }
    let autoresizes: bool = msg![env; view autoresizesSubviews];
    let subviews = env.objc.borrow::<UIViewHostObject>(view).subviews.clone();
    for subview in subviews {
        let frame: CGRect = msg![env; subview frame];
        let mask: NSUInteger = msg![env; subview autoresizingMask];

        let new_frame = if view_has_eagl_layer(env, subview) && frame_covers(frame, old_size) {
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: new_size,
            }
        } else if autoresizes && mask != 0 {
            autoresized_frame(frame, mask, old_size, new_size)
        } else {
            continue;
        };
        if new_frame == frame {
            continue;
        }

        log!(
            "Landscape root view resize: {:?} frame {:?} -> {:?} (autoresizingMask {:#x})",
            subview,
            frame,
            new_frame,
            mask
        );
        () = msg![env; subview setFrame:new_frame];
        resized.push(subview);

        let subview_old_size = frame.size;
        let subview_new_size: CGRect = msg![env; subview bounds];
        resize_subviews_for_rotation(
            env,
            subview,
            subview_old_size,
            subview_new_size.size,
            depth + 1,
            resized,
        );
    }
}

/// Finish the iOS 6-style autorotation of a root view controller's view whose
/// bounds just changed from `old_root_size` (portrait) to its rotated size:
/// resize its subviews as UIKit would, then run the layout pass the game would
/// get on a device, so engines that (re)create their GL framebuffer in
/// `-layoutSubviews` or `-viewWillLayoutSubviews` pick up the landscape size.
fn propagate_root_view_rotation(env: &mut Environment, vc: id, view: id, old_root_size: CGSize) {
    let new_root_bounds: CGRect = msg![env; view bounds];
    let new_root_size = new_root_bounds.size;
    log!(
        "Landscape root view resize: root view {:?} bounds {:?} -> {:?}",
        view,
        old_root_size,
        new_root_size
    );

    let mut resized = Vec::new();
    resize_subviews_for_rotation(env, view, old_root_size, new_root_size, 0, &mut resized);

    if vc != nil {
        () = msg![env; vc viewWillLayoutSubviews];
    }
    () = msg![env; view layoutSubviews];
    for resized_view in resized {
        () = msg![env; resized_view layoutSubviews];
    }
    if vc != nil {
        () = msg![env; vc viewDidLayoutSubviews];
    }
}

/// `-[EAGLContext renderbufferStorage:fromDrawable:]` hook, used when
/// [crate::wants_landscape_root_view_resize]: if the drawable's view still
/// has the portrait size of the screen while its superview is the
/// (autorotated) landscape root view, resize it to fill the superview before
/// the renderbuffer size is taken from it. This covers EAGL views that are
/// created after autorotation from `-[UIScreen bounds]`, which stays portrait
/// on iOS 6.
pub fn fit_eagl_view_to_landscape_superview(env: &mut Environment, drawable: id) {
    let view: id = msg![env; drawable delegate];
    if view == nil {
        return;
    }
    let superview: id = msg![env; view superview];
    if superview == nil {
        return;
    }
    let transform: CGAffineTransform = msg![env; view transform];
    if transform != CGAffineTransformIdentity {
        return;
    }
    let view_bounds: CGRect = msg![env; view bounds];
    let superview_bounds: CGRect = msg![env; superview bounds];
    let (view_w, view_h) = (view_bounds.size.width, view_bounds.size.height);
    let (super_w, super_h) = (superview_bounds.size.width, superview_bounds.size.height);
    let view_is_portrait_screen =
        view_h > view_w && nearly(view_w, super_h) && nearly(view_h, super_w);
    if !(super_w > super_h && view_is_portrait_screen) {
        return;
    }
    let new_frame = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: super_w,
            height: super_h,
        },
    };
    log!(
        "Landscape root view resize: EAGL view {:?} is {}x{} inside a {}x{} landscape superview; \
         resizing it to fill the superview before allocating its renderbuffer.",
        view,
        view_w,
        view_h,
        super_w,
        super_h
    );
    () = msg![env; view setFrame:new_frame];
}

#[cfg(test)]
mod landscape_root_view_tests {
    use super::*;

    fn rect(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize { width, height },
        }
    }

    const PORTRAIT: CGSize = CGSize {
        width: 768.0,
        height: 1024.0,
    };
    const LANDSCAPE: CGSize = CGSize {
        width: 1024.0,
        height: 768.0,
    };

    #[test]
    fn flexible_size_fills_rotated_parent() {
        let mask = UI_VIEW_AUTORESIZING_FLEXIBLE_WIDTH | UI_VIEW_AUTORESIZING_FLEXIBLE_HEIGHT;
        let new = autoresized_frame(rect(0.0, 0.0, 768.0, 1024.0), mask, PORTRAIT, LANDSCAPE);
        assert_eq!(new, rect(0.0, 0.0, 1024.0, 768.0));
    }

    #[test]
    fn fixed_margins_are_kept() {
        let mask = UI_VIEW_AUTORESIZING_FLEXIBLE_WIDTH | UI_VIEW_AUTORESIZING_FLEXIBLE_HEIGHT;
        let new = autoresized_frame(rect(10.0, 20.0, 748.0, 984.0), mask, PORTRAIT, LANDSCAPE);
        assert_eq!(new, rect(10.0, 20.0, 1004.0, 728.0));
    }

    #[test]
    fn flexible_margins_keep_a_view_centred() {
        let mask = UI_VIEW_AUTORESIZING_FLEXIBLE_LEFT_MARGIN
            | UI_VIEW_AUTORESIZING_FLEXIBLE_RIGHT_MARGIN
            | UI_VIEW_AUTORESIZING_FLEXIBLE_TOP_MARGIN
            | UI_VIEW_AUTORESIZING_FLEXIBLE_BOTTOM_MARGIN;
        let new = autoresized_frame(rect(284.0, 412.0, 200.0, 200.0), mask, PORTRAIT, LANDSCAPE);
        assert_eq!(new, rect(412.0, 284.0, 200.0, 200.0));
    }

    #[test]
    fn no_mask_means_no_change() {
        let frame = rect(0.0, 0.0, 768.0, 1024.0);
        assert_eq!(autoresized_frame(frame, 0, PORTRAIT, LANDSCAPE), frame);
    }

    #[test]
    fn covers_detects_full_screen_frames_only() {
        assert!(frame_covers(rect(0.0, 0.0, 768.0, 1024.0), PORTRAIT));
        assert!(!frame_covers(rect(0.0, 20.0, 768.0, 1004.0), PORTRAIT));
        assert!(!frame_covers(rect(0.0, 0.0, 1024.0, 768.0), PORTRAIT));
    }
}

/// Window life-cycle notifications.
///
/// Apple `UIWindow.h` declares each name as
/// `UIKIT_EXTERN NSNotificationName const ...`. The literal value
/// posted to `NSNotificationCenter` is the symbol's own name, which is
/// what `-[NSNotificationCenter addObserver:selector:name:object:]`
/// compares against with `-[NSString isEqualToString:]`.
///
/// References:
/// * Apple [UIWindow notifications](https://developer.apple.com/documentation/uikit/uiwindow)
const UIWindowDidBecomeKeyNotification: &str = "UIWindowDidBecomeKeyNotification";
const UIWindowDidResignKeyNotification: &str = "UIWindowDidResignKeyNotification";
const UIWindowDidBecomeHiddenNotification: &str = "UIWindowDidBecomeHiddenNotification";
const UIWindowDidBecomeVisibleNotification: &str = "UIWindowDidBecomeVisibleNotification";
/// Keyboard notifications
/// TODO: more keyboard notifications
pub const UIKeyboardWillShowNotification: &str = "UIKeyboardWillShowNotification";
pub const UIKeyboardDidShowNotification: &str = "UIKeyboardDidShowNotification";
pub const UIKeyboardWillHideNotification: &str = "UIKeyboardWillHideNotification";
pub const UIKeyboardDidHideNotification: &str = "UIKeyboardDidHideNotification";
pub const UIKeyboardBoundsUserInfoKey: &str = "UIKeyboardBoundsUserInfoKey";

pub const CONSTANTS: ConstantExports = &[
    (
        "_UIWindowDidBecomeKeyNotification",
        HostConstant::NSString(UIWindowDidBecomeKeyNotification),
    ),
    (
        "_UIWindowDidResignKeyNotification",
        HostConstant::NSString(UIWindowDidResignKeyNotification),
    ),
    (
        "_UIWindowDidBecomeHiddenNotification",
        HostConstant::NSString(UIWindowDidBecomeHiddenNotification),
    ),
    (
        "_UIWindowDidBecomeVisibleNotification",
        HostConstant::NSString(UIWindowDidBecomeVisibleNotification),
    ),
    // _UIKeyboardWillShowNotification, _UIKeyboardDidShowNotification,
    // _UIKeyboardWillHideNotification, _UIKeyboardDidHideNotification and
    // _UIKeyboardBoundsUserInfoKey are exported from
    // uikit::ui_keyboard::CONSTANTS; not duplicated here.
];
