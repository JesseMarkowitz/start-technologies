import { Component, inject, signal } from '@angular/core'
import {
  NonNullableFormBuilder,
  ReactiveFormsModule,
  Validators,
} from '@angular/forms'
import { tuiMarkControlAsTouchedAndValidate } from '@taiga-ui/cdk'
import {
  TuiButton,
  TuiCheckbox,
  TuiDialogContext,
  TuiError,
  TuiIcon,
  TuiInput,
  TuiLabel,
  TuiLink,
  TuiTextfield,
  TuiTitle,
} from '@taiga-ui/core'
import { TuiPassword } from '@taiga-ui/kit'
import { TuiForm, TuiHeader } from '@taiga-ui/layout'
import { injectContext, PolymorpheusComponent } from '@taiga-ui/polymorpheus'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { provideTranslatedValidationErrors } from 'src/app/i18n/validation-errors'
import { AccessPointCandidate } from 'src/app/services/api/api.service'

export type AdoptResult =
  | { name: string; secret: string }
  | { remove: string[] }

export interface AdoptData {
  candidate: AccessPointCandidate
  /** Saved configurations: names of access points out of contact. */
  saved: string[]
}

@Component({
  template: `
    <form tuiForm="m" [formGroup]="form" (submit.prevent)="adopt()">
      <header tuiHeader>
        <hgroup tuiTitle>
          <h2 [id]="context.id">{{ 'Adopt Access Point' | i18n }}</h2>
          <p>
            {{ context.data.candidate.address }} ·
            {{ context.data.candidate.version }}
          </p>
        </hgroup>
      </header>
      <tui-textfield>
        <label tuiLabel>{{ 'Name' | i18n }}</label>
        <input tuiInput formControlName="name" placeholder="garage" />
      </tui-textfield>
      <tui-error formControlName="name" />
      <p class="g-secondary">
        {{
          'A short name: up to 11 lowercase letters, digits and dashes.' | i18n
        }}
      </p>
      @if (context.data.saved.length) {
        <p class="g-secondary">
          {{ 'Or take over a saved configuration:' | i18n }}
          @for (name of context.data.saved; track name) {
            <button
              tuiLink
              type="button"
              (click)="form.controls.name.setValue(name)"
            >
              {{ name }}
            </button>
          }
        </p>
        <fieldset>
          <legend>{{ 'Delete saved configurations' | i18n }}</legend>
          @for (name of context.data.saved; track name) {
            <label tuiLabel>
              <input
                type="checkbox"
                tuiCheckbox
                [checked]="selected().includes(name)"
                (change)="toggle(name)"
              />
              {{ name }}
            </label>
          }
          <button
            tuiButton
            type="button"
            size="s"
            appearance="secondary-destructive"
            [disabled]="!selected().length"
            (click)="context.completeWith({ remove: selected() })"
          >
            {{ 'Delete selected' | i18n }}
          </button>
        </fieldset>
      }
      <tui-textfield>
        <label tuiLabel>
          {{
            (context.data.candidate.hasSticker
              ? 'Sticker Wi-Fi password'
              : 'Adoption code'
            ) | i18n
          }}
        </label>
        <input tuiInput type="password" formControlName="secret" />
        <tui-icon tuiPassword />
      </tui-textfield>
      <tui-error formControlName="secret" />
      <p class="g-secondary">
        {{
          (context.data.candidate.hasSticker
            ? 'The Wi-Fi password printed on the router you are adopting.'
            : 'The code shown on the router you are adopting.'
          ) | i18n
        }}
      </p>
      <footer>
        <button
          tuiButton
          appearance="flat"
          type="button"
          (click)="context.$implicit.complete()"
        >
          {{ 'Cancel' | i18n }}
        </button>
        <button tuiButton>{{ 'Adopt' | i18n }}</button>
      </footer>
    </form>
  `,
  providers: [
    provideTranslatedValidationErrors({
      required: 'Required',
      pattern: 'A short name: up to 11 lowercase letters, digits and dashes.',
    }),
  ],
  imports: [
    ReactiveFormsModule,
    TuiForm,
    TuiHeader,
    TuiTitle,
    TuiTextfield,
    TuiLabel,
    TuiInput,
    TuiError,
    TuiIcon,
    TuiPassword,
    TuiButton,
    TuiCheckbox,
    TuiLink,
    i18nPipe,
  ],
})
export class AdoptDialog {
  protected readonly context =
    injectContext<TuiDialogContext<AdoptResult, AdoptData>>()

  protected readonly selected = signal<string[]>([])

  protected readonly form = inject(NonNullableFormBuilder).group({
    name: ['', [Validators.required, Validators.pattern(/^[a-z0-9-]{1,11}$/)]],
    secret: ['', Validators.required],
  })

  toggle(name: string) {
    this.selected.update(s =>
      s.includes(name) ? s.filter(n => n !== name) : [...s, name],
    )
  }

  adopt() {
    tuiMarkControlAsTouchedAndValidate(this.form)
    if (this.form.invalid) return
    this.context.completeWith(this.form.getRawValue())
  }
}

export const ADOPT_DIALOG = new PolymorpheusComponent(AdoptDialog)
