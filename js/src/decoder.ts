import { Handler } from "./parser";

export class Decoder implements Handler<object> {
  private stack: object[];

  constructor() {
    this.stack = [{}];
  }

  // Add a uint64 to the current object
  uint64(id: number, value: number) {
    this.currentObject()[id] = value;
  }

  // Add binary data to the current object
  binary(id: number, value: Uint8Array) {
    this.currentObject()[id] = value;
  }

  // Push down the internal stack, constructing a new object
  beginNested() {
    this.stack.push({});
  }

  // End a nested object, setting it to the given ID on its parent
  endNested(id: number) {
    let value = this.stack.pop();

    if (this.stack.length === 0) {
      throw new Error("not inside a nested message");
    }

    this.currentObject()[id] = value;
  }

  // Finish decoding, returning the finished parent object
  finish(): any {
    let result = this.stack.pop();

    if (this.stack.length !== 0) {
      throw new Error("objects remaining in stack");
    }

    return result;
  }

  private currentObject(): any {
    return this.stack[this.stack.length - 1];
  }
}