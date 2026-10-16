import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { App } from "./App";
import { AddDialog } from "./components/AddDialog";
import { DownloadList } from "./components/DownloadList";
import "./i18n";
import type { AddInput, DesktopApi, Download } from "./types";

function row(index: number): Download {
  return {
    id: `id-${index}`,
    url: `https://example.test/${index}`,
    filename: `file-${index}.bin`,
    save_dir: "C:\\Downloads",
    status: "downloading",
    resumable: true,
    total_bytes: 1_000_000,
    downloaded_bytes: index,
    error_code: null,
    error_message: null,
  };
}

function api(overrides: Partial<DesktopApi> = {}): DesktopApi {
  return {
    mode: async () => "owner",
    list: async () => [],
    add: async () => ({ id: "created" }),
    pause: async () => {},
    resume: async () => {},
    remove: async () => {},
    segments: async () => [],
    settings: async () => ({}),
    setSetting: async () => {},
    defaultSaveDir: async () => "C:\\Downloads",
    subscribe: async () => () => {},
    ...overrides,
  };
}

describe("AddDialog", () => {
  it("keeps insecure TLS off until the warning is confirmed", () => {
    render(<AddDialog api={api()} open initialDir="C:\\Downloads" onClose={() => {}} onAdded={() => {}} />);
    const box = screen.getByRole("checkbox", { name: "Accept any TLS certificate" });
    fireEvent.click(box);
    expect(box).not.toBeChecked();
    expect(screen.getByRole("dialog", { name: "Skip TLS verification?" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Go back" }));
    expect(box).not.toBeChecked();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    fireEvent.click(box);
    fireEvent.click(screen.getByRole("button", { name: "Continue without verification" }));
    expect(box).toBeChecked();
  });

  it("retries an ask collision as rename or overwrite", async () => {
    const calls: AddInput[] = [];
    const add = vi.fn(async (input: AddInput) => {
      calls.push(input);
      if (input.duplicatePolicy === "ask") {
        throw { code: "duplicate_ask", message: "a filename already exists and the duplicate policy is ask" };
      }
      return { id: "dl-1" };
    });
    const onAdded = vi.fn();
    render(<AddDialog api={api({ add })} open initialDir="C:\\Downloads" onClose={() => {}} onAdded={onAdded} />);
    fireEvent.change(screen.getByLabelText("URL"), { target: { value: "https://example.test/a.bin" } });
    fireEvent.change(screen.getByLabelText("If the filename exists"), { target: { value: "ask" } });
    fireEvent.click(screen.getByRole("button", { name: "Start download" }));
    expect(await screen.findByRole("dialog", { name: "A file with this name already exists" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Overwrite" }));
    await vi.waitFor(() => expect(onAdded).toHaveBeenCalledWith("dl-1"));
    expect(calls.map((call) => call.duplicatePolicy)).toEqual(["ask", "overwrite"]);
    expect(calls[1].url).toBe("https://example.test/a.bin");
    expect(calls[1].insecureTls).toBe(false);
  });
});

describe("DownloadList", () => {
  it("renders only the visible window", () => {
    const rows = Array.from({ length: 1000 }, (_, index) => row(index));
    const { rerender } = render(
      <DownloadList rows={rows} selectedId={null} onSelect={() => {}} viewportHeight={400} scrollTop={0} />,
    );
    expect(screen.getByText("file-0.bin")).toBeInTheDocument();
    expect(screen.getByText("file-10.bin")).toBeInTheDocument();
    expect(screen.queryByText("file-11.bin")).not.toBeInTheDocument();
    expect(screen.queryByText("file-50.bin")).not.toBeInTheDocument();

    rerender(<DownloadList rows={rows} selectedId={null} onSelect={() => {}} viewportHeight={400} scrollTop={1000} />);
    expect(screen.queryByText("file-0.bin")).not.toBeInTheDocument();
    expect(screen.getByText("file-15.bin")).toBeInTheDocument();
    expect(screen.queryByText("file-50.bin")).not.toBeInTheDocument();
  });
});

describe("App", () => {
  it("tells a bare vite window that downloads need the desktop shell", async () => {
    render(<App api={api({ mode: async () => "shell" })} />);
    expect(await screen.findByText(/desktop shell/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add download" })).toBeDisabled();
  });

  it("shows a forward-mode banner and loads settings", async () => {
    const setSetting = vi.fn(async () => {});
    render(
      <App
        api={api({
          mode: async () => "forward",
          settings: async () => ({ user_agent: "ForgeDownloadManager/0.1" }),
          setSetting,
        })}
      />,
    );
    expect(await screen.findByText(/running engine/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const field = await screen.findByRole("textbox", { name: "User agent" });
    expect(field).toHaveValue("ForgeDownloadManager/0.1");
    fireEvent.change(field, { target: { value: "Custom/1" } });
    fireEvent.click(within(field.parentElement as HTMLElement).getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(setSetting).toHaveBeenCalledWith("user_agent", "Custom/1"));
  });
});
