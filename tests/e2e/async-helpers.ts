const NEXT_INDEX = 1;

export const delay = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

/**
 * Runs `run` for each item in order, awaiting each call before starting the next.
 * Recursive rather than loop-based so sequential awaits stay explicit.
 */
export const sequentially = async <T>(
  items: readonly T[],
  run: (item: T) => Promise<void>,
  index = 0,
): Promise<void> => {
  const item = items[index];
  if (item === undefined) {
    return;
  }
  await run(item);
  await sequentially(items, run, index + NEXT_INDEX);
};

/**
 * Repeats `attempt` until it returns a non-null value or `deadline` passes.
 * Returns null on timeout.
 */
export const pollUntil = async <T>(
  attempt: () => T | null | Promise<T | null>,
  intervalMs: number,
  deadline: number,
): Promise<T | null> => {
  if (Date.now() >= deadline) {
    return null;
  }
  const value = await attempt();
  if (value !== null) {
    return value;
  }
  await delay(intervalMs);
  return pollUntil(attempt, intervalMs, deadline);
};
