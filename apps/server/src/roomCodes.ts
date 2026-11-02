import { randomInt } from 'node:crypto';

const LETTERS = 'abcdefghijklmnopqrstuvwxyz';
const DIGITS = '0123456789';
const CODE_PART_LENGTH = 3;

const randomPart = (source: string, length: number): string => {
  let result = '';
  for (let index = 0; index < length; index++) {
    result += source.charAt(randomInt(source.length));
  }
  return result;
};

export const generateRoomCode = (): string =>
  `${randomPart(LETTERS, CODE_PART_LENGTH)}-${randomPart(DIGITS, CODE_PART_LENGTH)}-${randomPart(LETTERS, CODE_PART_LENGTH)}`;
