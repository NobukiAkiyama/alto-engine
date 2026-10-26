import { EngineCore, type Engine } from "./engine.js";
import { AltoError } from "./errors.js";

/** @internal The engine behind the public interface, for the unstable and testing entry points. */
export function coreOf(engine: Engine): EngineCore {
  if (!(engine instanceof EngineCore)) throw new AltoError("INVALID_ARGUMENT", "not an engine from createEngine");
  return engine;
}
