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
import { profile, showsFocusRing as profileShowsFocusRing } from "../input";

type FocusableProps = {
  id: string;
  onActivate?: () => void;
  onFocus?: () => void;
  onBlur?: () => void;
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
  }) => JSX.Element;
};

export const Focusable: Component<FocusableProps> = (props) => {
  let element: HTMLElement | null = null;
  let unregister: (() => void) | null = null;

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
    // Touch / mouse click claims focus AND activates.
    if (element) {
      setFocusedId(props.id);
    }
    props.onActivate?.();
  };

  onCleanup(() => {
    unregister?.();
    unregister = null;
  });

  // The render-prop receives reactive accessors, so the call site
  // returns reactive JSX even though `props.children` itself is a
  // plain function reference. eslint-plugin-solid can't see across
  // the function-call boundary; this is the documented pattern.
  // eslint-disable-next-line solid/reactivity
  return props.children({ focused, showRing, ref, onClick });
};
