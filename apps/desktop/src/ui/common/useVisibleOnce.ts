import { useEffect, useState } from "react";
import type { RefObject } from "react";

/**
 * True once the element has been in (or near) the viewport; it stays true.
 * Used to load images lazily. Without IntersectionObserver it is true at once.
 */
export function useVisibleOnce(ref: RefObject<Element | null>, rootMargin = "200px"): boolean {
  const [visible, setVisible] = useState(() => typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const el = ref.current;
    if (visible || !el) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setVisible(true);
      },
      { rootMargin },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref, visible, rootMargin]);
  return visible;
}
