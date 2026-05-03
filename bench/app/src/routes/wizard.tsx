import { useState } from 'react';

type Step = 0 | 1 | 2 | 3;

interface State {
  account: { username: string; password: string };
  profile: { displayName: string; tagline: string };
  preferences: { theme: 'dark' | 'light'; notifications: boolean };
}

export function Wizard() {
  const [step, setStep] = useState<Step>(0);
  const [s, setS] = useState<State>({
    account: { username: '', password: '' },
    profile: { displayName: '', tagline: '' },
    preferences: { theme: 'dark', notifications: true },
  });

  const next = () => setStep((p) => Math.min(3, (p + 1) as Step));
  const back = () => setStep((p) => Math.max(0, (p - 1) as Step));

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold" data-testid="wizard-title">
        Sign up wizard
      </h1>
      <div className="card">
        <div className="flex items-center gap-2 mb-4 text-sm" data-testid="wizard-stepper">
          {['account', 'profile', 'preferences', 'review'].map((label, i) => (
            <span
              key={label}
              data-testid={`step-${i}`}
              data-current={step === i}
              className={
                'px-2 py-0.5 rounded ' +
                (i <= step ? 'bg-[var(--accent)] text-white' : 'bg-[var(--border)]')
              }
            >
              {i + 1}. {label}
            </span>
          ))}
        </div>

        {step === 0 && (
          <div className="space-y-3" data-testid="step-content-0">
            <label className="block">
              <span className="text-sm">username</span>
              <input
                className="input"
                data-testid="wiz-username"
                value={s.account.username}
                onChange={(e) =>
                  setS({ ...s, account: { ...s.account, username: e.target.value } })
                }
              />
            </label>
            <label className="block">
              <span className="text-sm">password</span>
              <input
                className="input"
                type="password"
                data-testid="wiz-password"
                value={s.account.password}
                onChange={(e) =>
                  setS({ ...s, account: { ...s.account, password: e.target.value } })
                }
              />
            </label>
          </div>
        )}

        {step === 1 && (
          <div className="space-y-3" data-testid="step-content-1">
            <label className="block">
              <span className="text-sm">display name</span>
              <input
                className="input"
                data-testid="wiz-display"
                value={s.profile.displayName}
                onChange={(e) =>
                  setS({ ...s, profile: { ...s.profile, displayName: e.target.value } })
                }
              />
            </label>
            <label className="block">
              <span className="text-sm">tagline</span>
              <input
                className="input"
                data-testid="wiz-tagline"
                value={s.profile.tagline}
                onChange={(e) =>
                  setS({ ...s, profile: { ...s.profile, tagline: e.target.value } })
                }
              />
            </label>
          </div>
        )}

        {step === 2 && (
          <div className="space-y-3" data-testid="step-content-2">
            <label className="block">
              <span className="text-sm">theme</span>
              <select
                className="input"
                data-testid="wiz-theme"
                value={s.preferences.theme}
                onChange={(e) =>
                  setS({
                    ...s,
                    preferences: { ...s.preferences, theme: e.target.value as 'dark' | 'light' },
                  })
                }
              >
                <option value="dark">dark</option>
                <option value="light">light</option>
              </select>
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                data-testid="wiz-notifications"
                checked={s.preferences.notifications}
                onChange={(e) =>
                  setS({
                    ...s,
                    preferences: { ...s.preferences, notifications: e.target.checked },
                  })
                }
              />
              receive notifications
            </label>
          </div>
        )}

        {step === 3 && (
          <div className="space-y-2 text-sm" data-testid="step-content-3">
            <div data-testid="review-username">username: {s.account.username}</div>
            <div data-testid="review-display">display: {s.profile.displayName}</div>
            <div data-testid="review-tagline">tagline: {s.profile.tagline}</div>
            <div data-testid="review-theme">theme: {s.preferences.theme}</div>
            <div data-testid="review-notif">
              notifications: {s.preferences.notifications ? 'on' : 'off'}
            </div>
          </div>
        )}

        <div className="flex gap-2 mt-6">
          <button
            className="btn-ghost btn"
            data-testid="wiz-back"
            disabled={step === 0}
            onClick={back}
          >
            back
          </button>
          <button
            className="btn ml-auto"
            data-testid="wiz-next"
            disabled={step === 3}
            onClick={next}
          >
            next
          </button>
        </div>
      </div>
    </div>
  );
}
