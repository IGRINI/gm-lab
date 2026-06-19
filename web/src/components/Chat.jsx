import { useRef, useState, useEffect, useCallback, forwardRef, useMemo } from "react";
import { Virtuoso } from "react-virtuoso";
import Message from "./Message.jsx";
import Scene from "./Scene.jsx";
import { ChatScrollContext } from "../chatScrollContext.js";
import { EntityRegistryContext } from "../entityContext.js";
import { NpcRosterContext } from "../npcContext.js";
import { StatusLabelsContext } from "../statusContext.js";

const List = forwardRef(function List({ className, ...props }, ref) {
  return <div ref={ref} {...props} className={"chat-inner " + (className || "")} />;
});

const Header = ({ context }) => (
  <div className="chat-inner">
    <Scene scene={context.scene} npcs={context.npcs} />
  </div>
);

const Footer = () => <div className="list-pad-bottom" />;

const VComponents = { List, Header, Footer };

export default function Chat({ messages, scene, npcs, entities, statusLabels }) {
  const virtuoso = useRef(null);
  const scrollerRef = useRef(null);
  const atBottomRef = useRef(true);
  const pausedRef = useRef(false);     // user scrolled up mid-stream -> suspend auto-stick
  const detachScroll = useRef(null);
  const [showDown, setShowDown] = useState(false);
  const [newCount, setNewCount] = useState(0);
  const lastLen = useRef(0);

  const scrollToBottom = useCallback((behavior = "auto") => {
    // Respect prefers-reduced-motion: never animate the jump-to-bottom for those users.
    const reduce = typeof window !== "undefined" && window.matchMedia
      && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    virtuoso.current?.scrollToIndex({ index: "LAST", align: "end", behavior: reduce ? "auto" : behavior });
  }, []);

  // Wire user-intent listeners onto Virtuoso's scroller. An upward wheel/drag suspends
  // auto-stick immediately, so streaming text stops yanking the view back down; reaching the
  // very bottom again resumes it. Distinguishing the gesture from our own programmatic
  // scrollToBottom is what the atBottomThreshold alone could not do fast enough.
  const handleScrollerRef = useCallback((el) => {
    scrollerRef.current = el;
    if (detachScroll.current) { detachScroll.current(); detachScroll.current = null; }
    if (!el) return;
    let touchY = 0;
    const onWheel = (e) => { if (e.deltaY < 0) pausedRef.current = true; };
    const onTouchStart = (e) => { touchY = e.touches[0] ? e.touches[0].clientY : 0; };
    const onTouchMove = (e) => {
      const y = e.touches[0] ? e.touches[0].clientY : 0;
      if (y > touchY + 2) pausedRef.current = true;   // finger drags down => content scrolls up
      touchY = y;
    };
    const onScroll = () => {
      // Re-attach only when BOTH Virtuoso and raw math agree we're at the bottom, so iOS
      // momentum overshoot near the bottom can't flip us back to pinned mid-flick.
      if (atBottomRef.current && el.scrollHeight - el.scrollTop - el.clientHeight < 8) {
        pausedRef.current = false;
      }
    };
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    el.addEventListener("scroll", onScroll, { passive: true });
    detachScroll.current = () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("scroll", onScroll);
    };
  }, []);

  const scrollCtx = useMemo(
    () => ({
      getScroller: () => scrollerRef.current,
      isAtBottom: () => atBottomRef.current,
      scrollToBottom: () => scrollToBottom("auto"),
    }),
    [scrollToBottom]
  );

  // Keep pinned to the bottom while the last message streams in (grows),
  // and count messages that arrive while the user is reading history.
  useEffect(() => {
    const grew = messages.length - lastLen.current;
    lastLen.current = messages.length;
    if (atBottomRef.current && !pausedRef.current) {
      scrollToBottom("auto");
    } else if (grew > 0) {
      setNewCount((n) => n + grew);
    }
  }, [messages, scrollToBottom]);

  const onAtBottom = useCallback((atBottom) => {
    atBottomRef.current = atBottom;
    setShowDown(!atBottom);
    if (atBottom) {
      pausedRef.current = false;
      setNewCount(0);
    }
  }, []);

  const context = useMemo(() => ({ scene, npcs }), [scene, npcs]);

  return (
    <ChatScrollContext.Provider value={scrollCtx}>
      <EntityRegistryContext.Provider value={entities}>
      <NpcRosterContext.Provider value={npcs}>
      <StatusLabelsContext.Provider value={statusLabels || {}}>
      <div className="chat">
        <Virtuoso
          ref={virtuoso}
          scrollerRef={handleScrollerRef}
          className="chat-scroller"
          data={messages}
          context={context}
          components={VComponents}
          computeItemKey={(_i, item) => item.id}
          itemContent={(_i, item) => (
            <div className="row">
              <Message m={item} />
            </div>
          )}
          alignToBottom
          skipAnimationFrameInResizeObserver
          atBottomThreshold={120}
          atBottomStateChange={onAtBottom}
          increaseViewportBy={{ top: 600, bottom: 600 }}
        />
        <button
          className={"scrolldown" + (showDown ? " show" : "")}
          onClick={() => { pausedRef.current = false; scrollToBottom("smooth"); }}
          title=""
          aria-label="Вниз"
        >
          ↓
          {newCount > 0 && <span className="badge">{newCount > 99 ? "99+" : newCount}</span>}
        </button>
      </div>
      </StatusLabelsContext.Provider>
      </NpcRosterContext.Provider>
      </EntityRegistryContext.Provider>
    </ChatScrollContext.Provider>
  );
}
