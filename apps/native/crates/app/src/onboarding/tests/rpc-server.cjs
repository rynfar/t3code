// Isolated Effect RPC fixture. Run only from the native onboarding protocol tests.
const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");
const store = path.resolve(__dirname, "../../../../../../../node_modules/.pnpm");
const pkg = fs.readdirSync(store).find((name) => name.startsWith("ws@"));
const WebSocketServer = require(path.join(store, pkg, "node_modules/ws")).Server;
const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
const commands = new Set();
let creates = 0,
  imports = 0,
  interrupts = 0,
  createCalls = 0;
server.on("listening", () => console.log(server.address().port));
server.on("connection", (socket) =>
  socket.on("message", (data) => {
    for (const request of [JSON.parse(data)].flat()) {
      const send = (value) => socket.send(JSON.stringify(value));
      const ok = (value) =>
        send({ _tag: "Exit", requestId: request.id, exit: { _tag: "Success", value } });
      const fail = (message, tag = "FixtureError", extra = {}) =>
        send({
          _tag: "Exit",
          requestId: request.id,
          exit: {
            _tag: "Failure",
            cause: [{ _tag: "Fail", error: { _tag: tag, message, ...extra } }],
          },
        });
      if (request._tag === "Ping") {
        send({ _tag: "Pong" });
        continue;
      }
      if (request._tag === "Interrupt") {
        interrupts++;
        continue;
      }
      if (request._tag !== "Request") continue;
      switch (request.tag) {
        case "agentSessions.scan":
          if (request.payload.hold) break;
          ok({
            candidates: [
              {
                path: "/fixture/repo",
                title: "repo",
                sources: ["claudeAgent", "codex"],
                threadCount: 3,
                lastActiveAt: "2026-08-20T12:00:00.000Z",
              },
            ],
            scannedAt: "2026-08-22T12:00:00.000Z",
          });
          break;
        case "projects.mutate":
          assert.equal(request.payload.type, "project.create");
          assert.equal(request.payload.createWorkspaceRootIfMissing, false);
          assert.equal(request.payload.defaultModelSelection, null);
          assert.equal(
            request.payload.commandId,
            `onboarding:project:create:${request.payload.projectId}`,
          );
          createCalls++;
          if (request.payload.projectId === "rejected-project") {
            fail("Failed to mutate project.", "ProjectMutationError", {
              cause: {
                name: "ProjectConflictError",
                message: "Workspace belongs to another project.",
              },
            });
            break;
          }
          if (!commands.has(request.payload.commandId)) {
            commands.add(request.payload.commandId);
            creates++;
          }
          if (createCalls === 1)
            send({
              _tag: "Defect",
              defect: { message: "projection store unavailable after commit" },
            });
          else if (createCalls === 2)
            send({
              _tag: "ClientProtocolError",
              error: { message: "invalid response after commit" },
            });
          else if (createCalls === 3)
            fail("Failed to mutate project.", "ProjectMutationError", {
              cause: {
                name: "ProjectOperationError",
                message: "Project operation 'read-project' failed.",
              },
            });
          else if (createCalls === 4) fail("unknown outcome after commit", "FutureError");
          else ok({});
          break;
        case "agentSessions.import":
          if (request.payload.projectId === "held-project") break;
          assert.deepEqual(request.payload, {
            projectId: "stable-project",
            expectedWorkspaceRoot: "/fixture/repo",
          });
          imports++;
          ok({ importedCount: imports === 1 ? 2 : 3, skippedCount: imports === 1 ? 1 : 0 });
          break;
        case "fixture.inspect":
          ok({ creates, createCalls, imports, interrupts });
          break;
        default:
          fail("Unsupported method");
      }
    }
  }),
);
