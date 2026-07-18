import assert from "node:assert/strict";
import test from "node:test";

import { api, createTurnRequestId, streamTurn } from "../src/api.js";

test("turn cancellation is addressed by chat and request id", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  let request = null;
  globalThis.fetch = async (url, init) => {
    request = { url, init };
    return new Response('{"ok":true,"status":"cancelled","committed":false}', {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  await api.cancelTurn("chat/one", "turn/id");

  assert.equal(request.url, "/turn/turn%2Fid/cancel");
  assert.equal(request.init.method, "POST");
  assert.equal(request.init.headers.get("Accept-Language"), "ru");
  assert.deepEqual(JSON.parse(request.init.body), { chat_id: "chat/one" });
});

test("streamTurn sends request_id, forwards events, and returns terminal done", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const requestId = "47f31a77-fb20-44ba-9728-07cde80695bb";
  let request = null;
  globalThis.fetch = async (url, init) => {
    request = { url, init };
    return new Response(
      [
        'data: {"kind":"player","agent":"Игрок","data":"Осматриваюсь"}',
        "",
        `data: {"kind":"done","ok":true,"retryable":false,"replayed":false,"request_id":"${requestId}"}`,
        "",
      ].join("\n"),
      { status: 200, headers: { "Content-Type": "text/event-stream" } }
    );
  };

  const events = [];
  const done = await streamTurn("Осматриваюсь", requestId, (event) => events.push(event));

  assert.equal(request.url, "/turn");
  assert.equal(request.init.method, "POST");
  assert.deepEqual(JSON.parse(request.init.body), {
    text: "Осматриваюсь",
    request_id: requestId,
  });
  assert.equal(events.length, 1);
  assert.equal(events[0].kind, "player");
  assert.equal(done.ok, true);
  assert.equal(done.request_id, requestId);
});

test("streamTurn rejects a stream that closes without terminal done", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () =>
    new Response('data: {"kind":"player","data":"Иду"}\n\n', {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    });

  await assert.rejects(
    streamTurn("Иду", "9e4b507d-8f55-4570-91f6-1c77ae4dc0a8", () => {}),
    /до подтверждения хода/
  );
});

test("streamTurn forwards AbortSignal and stops the active request", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  let receivedSignal = null;
  globalThis.fetch = async (_url, init) => {
    receivedSignal = init.signal;
    return new Promise((_resolve, reject) => {
      const rejectAbort = () => {
        const error = new Error("aborted");
        error.name = "AbortError";
        reject(error);
      };
      if (init.signal.aborted) rejectAbort();
      else init.signal.addEventListener("abort", rejectAbort, { once: true });
    });
  };

  const controller = new AbortController();
  const pending = streamTurn(
    "Стой",
    "9e4b507d-8f55-4570-91f6-1c77ae4dc0a8",
    () => {},
    { signal: controller.signal }
  );
  controller.abort();

  await assert.rejects(pending, (error) => error.name === "AbortError");
  assert.equal(receivedSignal, controller.signal);
});

test("streamTurn marks only an explicit legacy resume in the request body", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const requestId = "47f31a77-fb20-44ba-9728-07cde80695bb";
  let body = null;
  globalThis.fetch = async (_url, init) => {
    body = JSON.parse(init.body);
    return new Response(
      `data: {"kind":"done","ok":true,"retryable":false,"replayed":false,"request_id":"${requestId}"}\n\n`,
      { status: 200, headers: { "Content-Type": "text/event-stream" } }
    );
  };

  await streamTurn("Осматриваюсь", requestId, () => {}, {
    legacyResume: true,
    chatId: "chat-main",
  });

  assert.deepEqual(body, {
    text: "Осматриваюсь",
    request_id: requestId,
    legacy_resume: true,
    chat_id: "chat-main",
  });
});

test("streamTurn sends a staged history mutation", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const requestId = "47f31a77-fb20-44ba-9728-07cde80695bb";
  let body = null;
  globalThis.fetch = async (_url, init) => {
    body = JSON.parse(init.body);
    return new Response(
      `data: {"kind":"done","ok":true,"retryable":false,"replayed":false,"request_id":"${requestId}"}\n\n`,
      { status: 200, headers: { "Content-Type": "text/event-stream" } }
    );
  };

  await streamTurn("Иду другим путём", requestId, () => {}, {
    chatId: "source-chat",
    history: { kind: "branch", turn: 3, title: "Другой путь" },
  });

  assert.deepEqual(body, {
    text: "Иду другим путём",
    request_id: requestId,
    chat_id: "source-chat",
    history: { kind: "branch", turn: 3, title: "Другой путь" },
  });
});

test("streamTurn hides raw server detail behind a localized safe fallback", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () =>
    new Response(JSON.stringify({ error: "Сервис временно недоступен" }), {
      status: 503,
      headers: { "Content-Type": "application/json" },
    });

  await assert.rejects(
    streamTurn("Иду", "9e4b507d-8f55-4570-91f6-1c77ae4dc0a8", () => {}),
    (error) =>
      error.retryable === true &&
      /ход не выполнен/.test(error.message) &&
      !/Сервис временно недоступен/.test(error.message)
  );
});

test("streamTurn marks ordinary client errors as non-retryable", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () =>
    new Response(JSON.stringify({ error: "Некорректный ход" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });

  await assert.rejects(
    streamTurn("Иду", "9e4b507d-8f55-4570-91f6-1c77ae4dc0a8", () => {}),
    (error) =>
      error.retryable === false &&
      /ход не выполнен/.test(error.message) &&
      !/Некорректный ход/.test(error.message)
  );
});

test("createTurnRequestId returns a UUID", () => {
  assert.match(
    createTurnRequestId(),
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
  );
});
