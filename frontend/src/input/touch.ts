// Touch-event watcher. The Web platform doesn't fire a clean
// "touch device connected" event, so we treat the first `touchstart`
// the app sees as the trigger to flip the capability flag. After
// that, the input profile resolver may switch to `touch` if the user
// override is `"auto"`.
//
// We do NOT translate raw touches into Actions here: PRD §F-017's
// touch column is "tap to focus / tap to activate", which is the
// browser's default behavior on `<button>` / clickable elements. The
// focus manager doesn't need to know about taps; the DOM element's
// click handler fires directly.

import { reportTouchPresent } from "./profile";

let installedListener: ((event: TouchEvent) => void) | null = null;

export function installTouchListener(target: Window = window): () => void {
  if (installedListener) return () => uninstallTouchListener(target);
  const listener = () => {
    reportTouchPresent(true);
  };
  target.addEventListener("touchstart", listener, { passive: true });
  installedListener = listener;
  return () => uninstallTouchListener(target);
}

export function uninstallTouchListener(target: Window = window): void {
  if (!installedListener) return;
  target.removeEventListener("touchstart", installedListener);
  installedListener = null;
}

/**
 * Test-only handle to drive the listener synchronously.
 */
export function handleTouchEvent(): void {
  reportTouchPresent(true);
}
