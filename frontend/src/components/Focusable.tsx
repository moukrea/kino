// Solid component that registers its host element with the focus
// manager and applies a visible focus class when this id is the
// currently-focused one.
//
// Usage:
//   <Focusable id="home-tile-1" onActivate={() => navigate("/title/1")}>
//     {(focused) => <div class={focused() ? "focused" : ""}>...</div>}
//   </Focusable>
//
// The render prop receives a reactive `focused()` accessor so the
// component can fold focus styling into its own template without
// reading global state directly.

import {
  createMemo,
  onCleanup,
  type Component,
  type JSX,
} from "solid-js";

import { focusedId, registerFocusable, setFocusedId } from "../input/focus";
import { onAction, profile, showsFocusRing as profileShowsFocusRing } from "../input";

/**
 * Long-press duration in ms (PRD §F-017 touch column: "long-press"
 * triggers the context action on touch screens). 500ms matches the
 * platform conventions for both Android and iOS WebViews.
 */
export const LONG_PRESS_MS = 500;

type FocusableProps = {
  id: string;
  onActivate?: () => void;
  onFocus?: () => void;
  onBlur?: () => void;
  /**
   * Optional context-action handler. Fired when:
   * - the user presses Y (gamepad) or Menu / F10 (D-pad / keyboard)
   *   while this Focusable holds focus (`context` action on the input
   *   bus, PRD §F-017),
   * - the user right-clicks the element (`contextmenu` browser event),
   * - the user long-presses the element on a touchscreen (>=
   *   `LONG_PRESS_MS` of held touch with no movement).
   * When unset, the right-click / long-press / Y events fall through
   * to the browser's default behavior.
   */
  onContext?: () => void;
  /**
   * Render prop receives a reactive `focused()` accessor and the
   * `ref` setter the host element must accept. The component is
   * render-prop-shaped because Solid doesn't have a clean way to
   * attach a ref + reactive context to arbitrary children.
   */
  children: (args: {
    focused: () => boolean;
    showRing: () => boolean;
    ref: (el: HTMLElement) => void;
    onClick: () => void;
    onContextMenu: (event: MouseEvent) => void;
    onTouchStart: () => void;
    onTouchEnd: () => void;
    onTouchMove: () => void;
    onTouchCancel: () => void;
  }) => JSX.Element;
};

export const Focusable: Component<FocusableProps> = (props) => {
  let element: HTMLElement | null = null;
  let unregister: (() => void) | null = null;
  let unsubscribeAction: (() => void) | null = null;
  let longPressTimer: ReturnType<typeof setTimeout> | null = null;
  let longPressFired = false;

  const focused = createMemo(() => focusedId() === props.id);
  const showRing = createMemo(() => focused() && profileShowsFocusRing(profile()));

  const ref = (el: HTMLElement) => {
    element = el;
    // Clean up the previous registration if HMR replaced the node.
    unregister?.();
    unregister = registerFocusable({
      id: props.id,
      element: el,
      onActivate: props.onActivate,
      onFocus: props.onFocus,
      onBlur: props.onBlur,
    });
  };

  const onClick = () => {
    // Touch / mouse click claims focus AND activates. Suppress when a
    // long-press just fired — Mobile Safari can emit `click` after the
    // long-press synthesis, and we don't want activation to trail the
    // context action.
    if (longPressFired) {
      longPressFired = false;
      return;
    }
    if (element) {
      setFocusedId(props.id);
    }
    props.onActivate?.();
  };

  const fireContext = () => {
    props.onContext?.();
  };

  const onContextMenu = (event: MouseEvent) => {
    if (!props.onContext) return;
    event.preventDefault();
    if (element) {
      setFocusedId(props.id);
    }
    fireContext();
  };

  const clearLongPress = () => {
    if (longPressTimer !== null) {
      clearTimeout(longPressTimer);
      longPressTimer = null;
    }
  };

  const onTouchStart = () => {
    if (!props.onContext) return;
    clearLongPress();
    longPressFired = false;
    longPressTimer = setTimeout(() => {
      longPressTimer = null;
      longPressFired = true;
      if (element) {
        setFocusedId(props.id);
      }
      fireContext();
    }, LONG_PRESS_MS);
  };

  const onTouchEnd = () => {
    clearLongPress();
  };

  const onTouchMove = () => {
    // Movement before the long-press timer expires aborts the
    // long-press — distinguishes a tap from a hold-and-drag.
    clearLongPress();
  };

  const onTouchCancel = () => {
    clearLongPress();
    longPressFired = false;
  };

  // Subscribe to the input bus so a `context` action emitted while
  // this Focusable holds focus invokes the handler. PRD §F-017
  // Y (gamepad) / Menu (D-pad) / F10 (keyboard) collapse to `context`.
  // Subscribed unconditionally so the cleanup symmetry is simple; the
  // handler short-circuits on unfocused or missing-callback. The
  // closure reads `props.onContext` / `props.id` on every emission so
  // prop updates flow through naturally — eslint-plugin-solid can't
  // see across the `onAction` boundary, hence the documented escape.
  // eslint-disable-next-line solid/reactivity
  unsubscribeAction = onAction((action) => {
    if (action !== "context") return;
    if (!props.onContext) return;
    if (focusedId() !== props.id) return;
    fireContext();
  });

  onCleanup(() => {
    unregister?.();
    unregister = null;
    unsubscribeAction?.();
    unsubscribeAction = null;
    clearLongPress();
  });

  // The render-prop receives reactive accessors, so the call site
  // returns reactive JSX even though `props.children` itself is a
  // plain function reference. eslint-plugin-solid can't see across
  // the function-call boundary; this is the documented pattern.
  // eslint-disable-next-line solid/reactivity
  return props.children({
    focused,
    showRing,
    ref,
    onClick,
    onContextMenu,
    onTouchStart,
    onTouchEnd,
    onTouchMove,
    onTouchCancel,
  });
};
