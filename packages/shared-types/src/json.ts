export type JsonObject = Readonly<Record<string, unknown>>;

export const isJsonObject = (value: unknown): value is JsonObject =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

export const readString = (value: JsonObject, key: string): string | undefined => {
  const field = value[key];
  if (typeof field === 'string') {
    return field;
  }
  return undefined;
};

export const readFiniteNumber = (value: JsonObject, key: string): number | undefined => {
  const field = value[key];
  if (typeof field === 'number' && Number.isFinite(field)) {
    return field;
  }
  return undefined;
};

export const readBoolean = (value: JsonObject, key: string): boolean | undefined => {
  const field = value[key];
  if (typeof field === 'boolean') {
    return field;
  }
  return undefined;
};
