import { describe, it, expect } from 'vitest';

import {
  narrowDaemonRecoveryPayload,
  narrowRecoveryStatusReport,
  BoundaryError,
} from './session-boundary';

describe('narrowDaemonRecoveryPayload', () => {
  it.each(['idle', 'retrying', 'exhausted'])(
    'accepts a valid payload with status "%s"',
    (status) => {
      const result = narrowDaemonRecoveryPayload({ status, rev: 1 });
      expect(result).toEqual({ status, rev: 1 });
    }
  );

  it('rejects a payload missing status', () => {
    expect(() => narrowDaemonRecoveryPayload({ rev: 1 })).toThrow(BoundaryError);
  });

  it('rejects a payload missing rev', () => {
    expect(() => narrowDaemonRecoveryPayload({ status: 'idle' })).toThrow(BoundaryError);
  });

  it('rejects a payload whose status is not a string', () => {
    expect(() => narrowDaemonRecoveryPayload({ status: 1, rev: 1 })).toThrow(BoundaryError);
  });

  it('rejects a payload whose status is an unrecognized value', () => {
    expect(() => narrowDaemonRecoveryPayload({ status: 'reconnecting', rev: 1 })).toThrow(
      BoundaryError
    );
  });

  it('rejects a payload whose rev is not a number', () => {
    expect(() => narrowDaemonRecoveryPayload({ status: 'idle', rev: '1' })).toThrow(BoundaryError);
  });

  it('rejects a payload whose rev is not an integer', () => {
    expect(() => narrowDaemonRecoveryPayload({ status: 'idle', rev: 1.5 })).toThrow(BoundaryError);
  });

  it('rejects a value that is not an object', () => {
    expect(() => narrowDaemonRecoveryPayload('idle')).toThrow(BoundaryError);
  });

  it('accepts rev 0 as a boundary value', () => {
    const result = narrowDaemonRecoveryPayload({ status: 'idle', rev: 0 });
    expect(result).toEqual({ status: 'idle', rev: 0 });
  });

  it('rejects a payload whose rev is negative', () => {
    expect(() => narrowDaemonRecoveryPayload({ status: 'idle', rev: -1 })).toThrow(BoundaryError);
  });
});

describe('narrowRecoveryStatusReport', () => {
  it('accepts a valid report with config_failure null', () => {
    const result = narrowRecoveryStatusReport({ status: 'idle', rev: 0, config_failure: null });
    expect(result).toEqual({
      status: 'idle',
      rev: 0,
      config_failure: null,
      startup_env_failure: null,
    });
  });

  it.each(['not_found', 'read_failed', 'invalid_toml', 'too_large'])(
    'accepts a valid report with config_failure "%s"',
    (failure) => {
      const result = narrowRecoveryStatusReport({
        status: 'retrying',
        rev: 2,
        config_failure: failure,
      });
      expect(result.config_failure).toBe(failure);
    }
  );

  it('treats an absent config_failure key as null', () => {
    const result = narrowRecoveryStatusReport({ status: 'exhausted', rev: 5 });
    expect(result.config_failure).toBeNull();
  });

  it('rejects a report missing status', () => {
    expect(() => narrowRecoveryStatusReport({ rev: 0, config_failure: null })).toThrow(
      BoundaryError
    );
  });

  it('rejects a report missing rev', () => {
    expect(() => narrowRecoveryStatusReport({ status: 'idle', config_failure: null })).toThrow(
      BoundaryError
    );
  });

  it('rejects a report whose status is an unrecognized value', () => {
    expect(() =>
      narrowRecoveryStatusReport({ status: 'bogus', rev: 0, config_failure: null })
    ).toThrow(BoundaryError);
  });

  it('rejects a report whose rev is not a number', () => {
    expect(() =>
      narrowRecoveryStatusReport({ status: 'idle', rev: 'zero', config_failure: null })
    ).toThrow(BoundaryError);
  });

  it('rejects a report whose config_failure is an unrecognized value', () => {
    expect(() =>
      narrowRecoveryStatusReport({ status: 'idle', rev: 0, config_failure: 'bogus' })
    ).toThrow(BoundaryError);
  });

  it('rejects a report whose config_failure has the wrong type', () => {
    expect(() =>
      narrowRecoveryStatusReport({ status: 'idle', rev: 0, config_failure: 42 })
    ).toThrow(BoundaryError);
  });

  it('rejects a value that is not an object', () => {
    expect(() => narrowRecoveryStatusReport(null)).toThrow(BoundaryError);
  });

  it('accepts rev 0 as a boundary value', () => {
    const result = narrowRecoveryStatusReport({ status: 'idle', rev: 0, config_failure: null });
    expect(result.rev).toBe(0);
  });

  it('rejects a report whose rev is negative', () => {
    expect(() =>
      narrowRecoveryStatusReport({ status: 'idle', rev: -1, config_failure: null })
    ).toThrow(BoundaryError);
  });
});

// The report gains a second optional reason, why
// start-up could not resolve this process's instance. Read the same way
// `config_failure` is -- an absent key or JSON `null` both mean "no failure".
describe('narrowRecoveryStatusReport: startup_env_failure', () => {
  it('accepts a valid report with startup_env_failure null', () => {
    const result = narrowRecoveryStatusReport({
      status: 'idle',
      rev: 0,
      config_failure: null,
      startup_env_failure: null,
    });
    expect(result).toEqual({
      status: 'idle',
      rev: 0,
      config_failure: null,
      startup_env_failure: null,
    });
  });

  it.each(['invalid_instance_id', 'store_override_with_named_instance'])(
    'accepts a valid report with startup_env_failure "%s"',
    (failure) => {
      const result = narrowRecoveryStatusReport({
        status: 'idle',
        rev: 0,
        config_failure: null,
        startup_env_failure: failure,
      });
      expect(result.startup_env_failure).toBe(failure);
    }
  );

  it('treats an absent startup_env_failure key as null', () => {
    const result = narrowRecoveryStatusReport({ status: 'idle', rev: 0 });
    expect(result.startup_env_failure).toBeNull();
  });

  it('rejects a report whose startup_env_failure is an unrecognized value', () => {
    expect(() =>
      narrowRecoveryStatusReport({
        status: 'idle',
        rev: 0,
        config_failure: null,
        startup_env_failure: 'bogus',
      })
    ).toThrow(BoundaryError);
  });

  it('rejects a report whose startup_env_failure has the wrong type', () => {
    expect(() =>
      narrowRecoveryStatusReport({
        status: 'idle',
        rev: 0,
        config_failure: null,
        startup_env_failure: 42,
      })
    ).toThrow(BoundaryError);
  });

  it('config_failure and startup_env_failure are read independently of one another', () => {
    const result = narrowRecoveryStatusReport({
      status: 'idle',
      rev: 0,
      config_failure: 'read_failed',
      startup_env_failure: 'invalid_instance_id',
    });
    expect(result.config_failure).toBe('read_failed');
    expect(result.startup_env_failure).toBe('invalid_instance_id');
  });
});
