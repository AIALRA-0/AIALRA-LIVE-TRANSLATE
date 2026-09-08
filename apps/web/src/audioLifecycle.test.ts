// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { BrowserCapture, testMicrophone } from "./audio";
afterEach(() => vi.unstubAllGlobals());

it("stops a permission grant arriving after the recorder was disposed", async () => {
  Object.defineProperty(window, "isSecureContext", {value:true,configurable:true});
  vi.stubGlobal("AudioContext", class {});
  vi.stubGlobal("AudioWorkletNode", class {});
  let grant!: (stream: MediaStream) => void;
  vi.stubGlobal("navigator", {mediaDevices:{getUserMedia:() => new Promise<MediaStream>((resolve) => {grant=resolve;})}});
  const stop=vi.fn();
  const stream={getTracks:()=>[{stop}]} as unknown as MediaStream;
  const recorder=new BrowserCapture("project-test","session-test","device-test",vi.fn());
  const prepare=recorder.prepare();
  recorder.dispose(); grant(stream);
  await expect(prepare).rejects.toThrow();
  expect(stop).toHaveBeenCalledTimes(1);
});

it("releases the microphone when audio context creation fails during testing", async () => {
  const stop=vi.fn();
  vi.stubGlobal("navigator", {mediaDevices:{getUserMedia:async()=>({getTracks:()=>[{stop}]})}});
  vi.stubGlobal("AudioContext", class {constructor(){throw new Error("context unavailable");}});
  await expect(testMicrophone(undefined,vi.fn())).rejects.toThrow("context unavailable");
  expect(stop).toHaveBeenCalledTimes(1);
});
