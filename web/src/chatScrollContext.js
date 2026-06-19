import { createContext } from "react";

// Provided by <Chat>. Lets descendants (spoilers) read the scroll element and
// anchor the bottom when their height changes.
//   getScroller()   -> the Virtuoso scroll element (or null)
//   isAtBottom()    -> boolean
//   scrollToBottom()-> jump/animate to the last message
export const ChatScrollContext = createContext({
  getScroller: () => null,
  isAtBottom: () => false,
  scrollToBottom: () => {},
});
