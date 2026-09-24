import { describe, expect, it, vi } from "vitest";
import type { CelerisClient } from "~/celeris/client.server";
import { CelerisError } from "~/celeris/errors";
import { loadMaintenance, submitMaintenance } from "~/routes/projects.$id.docs-maintenance";

describe("documentation maintenance human gate", () => {
  it("approval submits exactly the displayed plan and does not apply it", async () => {
    const post = vi.fn().mockResolvedValue({ approved: true });
    const client = { post } as unknown as CelerisClient;
    const form = new FormData();
    const plan = { revision: "abc", actions: [{ type: "delete", path: "docs/old.md" }] };
    form.set("op", "approve");
    form.set("plan", JSON.stringify(plan));
    const result = await submitMaintenance(client, "project", form);
    expect(result.error).toBeNull();
    expect(post).toHaveBeenCalledExactlyOnceWith(
      "/projects/project/docs/maintenance",
      { op: "approve", plan },
      { signal: undefined },
    );
  });
  it("invalid plan is not sent and stale approval is visible", async () => {
    const post = vi.fn().mockRejectedValue(new Error("stale approval: branch changed"));
    const client = { post } as unknown as CelerisClient;
    const form = new FormData();
    form.set("op", "apply");
    form.set("plan", "invalid JSON");
    expect((await submitMaintenance(client, "project", form)).error).not.toBeNull();
    expect(post).not.toHaveBeenCalled();
    form.set("plan", JSON.stringify({ revision: "old", actions: [] }));
    expect((await submitMaintenance(client, "project", form)).error).toContain("stale approval");
  });
});

describe("documentation maintenance availability", () => {
  it("renders unsupported repositories as an unavailable view without actionable data", async () => {
    const client = {
      get: vi
        .fn()
        .mockRejectedValue(
          new CelerisError({ status: 409, code: "docs_unavailable", detail: "local Git repository required" }),
        ),
    } as unknown as CelerisClient;
    const view = await loadMaintenance(client, "remote-project");
    expect(view.unavailable).toBe("local Git repository required");
    expect(view.audit).toBeNull();
    expect(view.proposal).toBeNull();
    expect(view.policy).toBeNull();
  });
  it.each(["unauthorized", "docs_maintenance", "project_not_found"])("preserves %s errors", async (code) => {
    const error = new CelerisError({
      status: code === "unauthorized" ? 401 : 409,
      code,
      detail: "must remain visible",
    });
    const client = { get: vi.fn().mockRejectedValue(error) } as unknown as CelerisClient;
    await expect(loadMaintenance(client, "project")).rejects.toBe(error);
  });
});
