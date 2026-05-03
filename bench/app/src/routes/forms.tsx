import { useState } from 'react';
import { useForm } from 'react-hook-form';
import { z } from 'zod';
import { zodResolver } from '@hookform/resolvers/zod';

const Schema = z.object({
  name: z.string().min(2, 'name must be at least 2 characters'),
  email: z.string().email('invalid email'),
  age: z.coerce.number().int().min(13, 'must be 13 or older').max(120, 'must be 120 or younger'),
  role: z.enum(['user', 'admin', 'guest']),
  bio: z.string().max(280, 'bio must be 280 characters or fewer'),
  agree: z.literal(true, { errorMap: () => ({ message: 'must accept terms' }) }),
});

type FormValues = z.infer<typeof Schema>;

export function FormsPage() {
  const [submitted, setSubmitted] = useState<FormValues | null>(null);
  const [submitCount, setSubmitCount] = useState(0);
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
    reset,
  } = useForm<FormValues>({
    resolver: zodResolver(Schema),
    defaultValues: { role: 'user', age: 18 } as Partial<FormValues>,
  });

  async function onSubmit(values: FormValues) {
    // Simulate async submit (matches a real form posting to an API).
    await new Promise((r) => setTimeout(r, 60));
    const r = await fetch('/api/submit', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(values),
    });
    if (!r.ok) throw new Error(`submit ${r.status}`);
    const echo = (await r.json()) as FormValues;
    setSubmitted(echo);
    setSubmitCount((n) => n + 1);
    reset({ role: 'user', age: 18 } as Partial<FormValues>);
  }

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold" data-testid="forms-title">
        Forms
      </h1>
      <form onSubmit={handleSubmit(onSubmit)} className="card space-y-3" data-testid="profile-form">
        <label className="block">
          <span className="text-sm">name</span>
          <input className="input" data-testid="form-name" {...register('name')} />
          {errors.name && (
            <div className="error" data-testid="error-name">
              {errors.name.message}
            </div>
          )}
        </label>
        <label className="block">
          <span className="text-sm">email</span>
          <input className="input" data-testid="form-email" {...register('email')} />
          {errors.email && (
            <div className="error" data-testid="error-email">
              {errors.email.message}
            </div>
          )}
        </label>
        <label className="block">
          <span className="text-sm">age</span>
          <input
            className="input"
            type="number"
            data-testid="form-age"
            {...register('age')}
          />
          {errors.age && (
            <div className="error" data-testid="error-age">
              {errors.age.message}
            </div>
          )}
        </label>
        <label className="block">
          <span className="text-sm">role</span>
          <select className="input" data-testid="form-role" {...register('role')}>
            <option value="user">user</option>
            <option value="admin">admin</option>
            <option value="guest">guest</option>
          </select>
          {errors.role && (
            <div className="error" data-testid="error-role">
              {errors.role.message}
            </div>
          )}
        </label>
        <label className="block">
          <span className="text-sm">bio</span>
          <textarea className="input min-h-24" data-testid="form-bio" {...register('bio')} />
          {errors.bio && (
            <div className="error" data-testid="error-bio">
              {errors.bio.message}
            </div>
          )}
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" data-testid="form-agree" {...register('agree')} />
          accept terms
          {errors.agree && (
            <div className="error" data-testid="error-agree">
              {errors.agree.message}
            </div>
          )}
        </label>
        <button className="btn" data-testid="form-submit" disabled={isSubmitting}>
          {isSubmitting ? 'submitting…' : 'submit'}
        </button>
      </form>

      {submitted && (
        <div className="card" data-testid="submit-result">
          <div className="text-sm text-[var(--fg)]/60">submitted ({submitCount})</div>
          <pre className="text-xs mt-2" data-testid="submit-payload">
            {JSON.stringify(submitted, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}
