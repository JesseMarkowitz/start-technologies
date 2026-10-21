import { Component, inject, linkedSignal, signal } from '@angular/core'
import { toSignal } from '@angular/core/rxjs-interop'
import { catchError, from, of, switchMap, timer } from 'rxjs'
import {
  AbstractControl,
  NonNullableFormBuilder,
  ReactiveFormsModule,
  ValidationErrors,
  Validators,
} from '@angular/forms'
import { tuiMarkControlAsTouchedAndValidate } from '@taiga-ui/cdk'
import {
  TuiButton,
  TuiError,
  TuiIcon,
  TuiInput,
  TuiLabel,
  TuiNotification,
  TuiTextfield,
  TuiTitle,
} from '@taiga-ui/core'
import { provideTranslatedValidationErrors } from 'src/app/i18n/validation-errors'
import { TuiButtonLoading, TuiPassword } from '@taiga-ui/kit'
import { TuiCardLarge, TuiForm, TuiHeader } from '@taiga-ui/layout'
import { ApiService } from 'src/app/services/api/api.service'
import { AuthService } from 'src/app/services/auth.service'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { getBrowserTimezone } from 'src/app/utils/timezones'

function passwordsMatch(control: AbstractControl): ValidationErrors | null {
  const password = control.get('password')?.value
  const confirm = control.get('confirm')?.value
  if (password && confirm && password !== confirm) {
    control.get('confirm')?.setErrors({ mismatch: true })
    return { mismatch: true }
  }
  return null
}

@Component({
  template: `
    <img alt="StartWRT" src="assets/favicon.svg" />
    @if (complete()) {
      <header tuiHeader>
        <hgroup tuiTitle>
          <h2>{{ 'Setup complete' | i18n }}</h2>
          <p tuiSubtitle>
            {{
              'Open router.lan in your browser to secure your connection to your router.'
                | i18n
            }}
          </p>
        </hgroup>
      </header>
      <a tuiButton href="http://router.lan" target="_blank">
        {{ 'Open router.lan' | i18n }}
      </a>
    } @else {
      @if (adoption(); as adoption) {
        @if (adoption.waiting && !adoption.heardMain) {
          <div tuiNotification appearance="neutral">
            <p>
              @if (!adoption.link) {
                {{ 'No cable is connected to the WAN port.' | i18n }}
              } @else if (adoption.internet) {
                {{
                  'The WAN port reaches the Internet, but no main StartWRT router answers on it.'
                    | i18n
                }}
              } @else {
                {{
                  'The WAN port has a cable, but no Internet connection and no main StartWRT router.'
                    | i18n
                }}
              }
            </p>
            <p>
              {{
                'To make this router an access point, connect its WAN port to a trunk port on your main router. To make it your main router, create a password below.'
                  | i18n
              }}
            </p>
          </div>
        }
        @if (adoption.waiting && adoption.heardMain) {
          <div tuiNotification appearance="info">
            <p>
              {{
                'This router hears a StartWRT router on its WAN port and can join it as an access point. To make it your main router instead, create a password below.'
                  | i18n
              }}
            </p>
            @if (adoption.code) {
              <p>
                {{ 'Adoption code' | i18n }}:
                <code>{{ adoption.code }}</code>
              </p>
            }
          </div>
        }
      }
      <header tuiHeader>
        <h2 tuiTitle>{{ 'Create admin password' | i18n }}</h2>
      </header>
      <form tuiForm="m" [formGroup]="form" (ngSubmit)="onSubmit()">
        <tui-textfield>
          <label tuiLabel>{{ 'Password' | i18n }}</label>
          <input tuiInput formControlName="password" type="password" />
          <tui-icon tuiPassword />
        </tui-textfield>
        <tui-error formControlName="password" />
        <tui-textfield>
          <label tuiLabel>{{ 'Confirm password' | i18n }}</label>
          <input tuiInput formControlName="confirm" type="password" />
          <tui-icon tuiPassword />
        </tui-textfield>
        <tui-error formControlName="confirm" />
        <tui-error [error]="error()" />
        <footer>
          <button tuiButton appearance="primary" [loading]="loading()">
            {{ 'Set Password' | i18n }}
          </button>
        </footer>
      </form>
    }
  `,
  styles: `
    :host {
      position: absolute;
      inline-size: 20rem;
      inset-inline-start: 50%;
      inset-block-start: 50%;
      transform: translate(-50%, -50%);
    }

    img {
      width: 5rem;
      height: 5rem;
      margin: auto;
    }

    :host-context(body:not([tuiTheme])) img {
      filter: invert(1);
    }
  `,
  hostDirectives: [TuiCardLarge],
  providers: [
    provideTranslatedValidationErrors({
      required: 'This field is required',
      minlength: 'Password must be at least 12 characters',
      mismatch: 'Passwords do not match',
    }),
  ],
  imports: [
    ReactiveFormsModule,
    TuiButton,
    TuiButtonLoading,
    TuiError,
    TuiIcon,
    TuiInput,
    TuiLabel,
    TuiNotification,
    TuiPassword,
    TuiTextfield,
    TuiHeader,
    TuiTitle,
    TuiForm,
    i18nPipe,
  ],
})
export default class Setup {
  private readonly api = inject(ApiService)
  private readonly auth = inject(AuthService)
  private readonly i18n = inject(i18nPipe)

  // A fresh router that hears a main router on its WAN port waits to be adopted
  // (design §19); the page says so and shows the code a board with no
  // sticker password needs.
  protected readonly adoption = toSignal(
    timer(0, 5000).pipe(
      switchMap(() =>
        from(this.api.accessPointAdoptionCode()).pipe(
          catchError(() => of(null)),
        ),
      ),
    ),
  )

  protected readonly loading = signal(false)
  protected readonly complete = signal(false)
  protected readonly error = linkedSignal<string | null>(() => null)

  public readonly form = inject(NonNullableFormBuilder).group(
    {
      password: ['', [Validators.required, Validators.minLength(12)]],
      confirm: ['', Validators.required],
    },
    { validators: passwordsMatch },
  )

  async onSubmit(): Promise<void> {
    if (this.form.invalid) {
      tuiMarkControlAsTouchedAndValidate(this.form)
      return
    }

    this.loading.set(true)
    this.error.set(null)

    try {
      await this.api.setInitialPassword({
        password: this.form.value.password!,
      })
      // Best-effort: persist the browser's zone as the default. The backend
      // resolves the POSIX string from its authoritative table; if the zone is
      // unknown it errors, which we swallow here (correctable later in Settings).
      await this.api
        .setTimezone({ timezone: getBrowserTimezone() })
        .catch(() => {})
      this.auth.initialized.set(true)
      this.complete.set(true)
    } catch (e: any) {
      this.error.set(
        e?.message || this.i18n.transform('Failed to set password'),
      )
    } finally {
      this.loading.set(false)
    }
  }
}
