import { Component, inject, input } from '@angular/core'
import { FormsModule } from '@angular/forms'
import { TuiResponsiveDialogService } from '@taiga-ui/addon-mobile'
import { TuiTable, TuiTableDirective } from '@taiga-ui/addon-table'
import { TuiButton, TuiDataList, TuiDropdown, TuiLink } from '@taiga-ui/core'
import {
  TUI_CONFIRM,
  TuiBadge,
  TuiButtonSelect,
  TuiChevron,
} from '@taiga-ui/kit'
import { filter } from 'rxjs'
import { Placeholder } from 'src/app/components/placeholder'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { AccessPoint } from 'src/app/services/api/api.service'

import { AccessPointsService } from './service'

const BAND = { '2g': '2.4 GHz', '5g': '5 GHz', '6g': '6 GHz' } as const

@Component({
  selector: '[accessPoints]',
  template: `
    <thead>
      <tr>
        <th tuiTh [sorter]="'name' | tuiSorter">{{ 'Name' | i18n }}</th>
        <th tuiTh>{{ 'Status' | i18n }}</th>
        <th tuiTh>{{ 'Trunk port' | i18n }}</th>
        <th tuiTh>{{ 'Radios' | i18n }}</th>
        <th tuiTh>{{ 'Ports' | i18n }}</th>
        <th tuiTh>{{ 'Devices' | i18n }}</th>
        <th tuiTh [style.width.rem]="3"></th>
      </tr>
    </thead>
    <tbody>
      @for (item of accessPoints() | tuiTableSort; track item.name) {
        <tr>
          <td tuiTd>
            <strong>{{ item.name }}</strong>
          </td>
          <td tuiTd>
            <span
              tuiBadge
              size="s"
              [appearance]="item.live ? 'positive' : 'negative'"
            >
              {{ (item.live ? 'In contact' : 'Out of contact') | i18n }}
            </span>
            <div class="g-secondary">
              {{ 'Generation' | i18n }} {{ item.generationApplied }}
            </div>
          </td>
          <td tuiTd>{{ item.port || '—' }}</td>
          <td tuiTd>
            @for (radio of plan(item); track radio.name) {
              <div>{{ radio.name }}: {{ radio.text }}</div>
            } @empty {
              <span class="g-secondary">—</span>
            }
          </td>
          <td tuiTd>
            @for (port of accessPorts(item); track port) {
              <div>
                {{ port }}:
                <button
                  tuiLink
                  tuiChevron
                  tuiButtonSelect
                  tuiDropdownAlign="start"
                  [textContent]="portLabel(item, port)"
                  [ngModel]="item.ports[port] ?? null"
                  (ngModelChange)="onPortChange(item, port, $event)"
                >
                  <tui-data-list *tuiDropdown>
                    <button tuiOption [value]="null">{{ 'Off' | i18n }}</button>
                    @for (p of service.profiles(); track p.vlan_tag) {
                      <button tuiOption [value]="p.vlan_tag">
                        {{ p.fullname }}
                      </button>
                    }
                  </tui-data-list>
                </button>
              </div>
            } @empty {
              <span class="g-secondary">—</span>
            }
          </td>
          <td tuiTd>{{ item.clients.length }}</td>
          <td tuiTd>
            <button
              tuiIconButton
              size="xs"
              iconStart="@tui.ellipsis-vertical"
              appearance="icon"
              tuiDropdownAlign="end"
              tuiDropdownAuto
              tuiDropdown
            >
              {{ 'Actions' | i18n }}
              <tui-data-list
                *tuiDropdown="let close"
                size="s"
                (click)="close()"
              >
                <button
                  tuiOption
                  iconEnd="@tui.trash"
                  class="g-negative"
                  (click)="remove(item)"
                >
                  {{ 'Remove' | i18n }}
                </button>
              </tui-data-list>
            </button>
          </td>
        </tr>
      } @empty {
        <tr>
          <td tuiTd colspan="7">
            <app-placeholder icon="@tui.radio">
              {{ 'No access points yet' | i18n }}
            </app-placeholder>
          </td>
        </tr>
      }
    </tbody>
  `,
  hostDirectives: [TuiTableDirective],
  host: { class: 'g-table' },
  imports: [
    TuiTable,
    TuiBadge,
    TuiButton,
    TuiLink,
    TuiChevron,
    TuiButtonSelect,
    TuiDataList,
    TuiDropdown,
    FormsModule,
    Placeholder,
    i18nPipe,
  ],
})
export class AccessPointsTable {
  protected readonly service = inject(AccessPointsService)
  private readonly dialogs = inject(TuiResponsiveDialogService)
  private readonly i18n = inject(i18nPipe)

  readonly accessPoints = input<AccessPoint[]>([])

  plan(item: AccessPoint): { name: string; text: string }[] {
    return Object.entries(item.plan).map(([name, p]) => ({
      name,
      text: `${BAND[p.band as keyof typeof BAND] ?? p.band} · ${p.channel} · ${p.htmode}`,
    }))
  }

  accessPorts(item: AccessPoint): string[] {
    return (item.inventory?.ports ?? []).filter(p => !p.uplink).map(p => p.name)
  }

  portLabel(item: AccessPoint, port: string): string {
    const vlan = item.ports[port]
    if (vlan === null || vlan === undefined) return this.i18n.transform('Off')
    return (
      this.service.profiles().find(p => p.vlan_tag === vlan)?.fullname ??
      `VLAN ${vlan}`
    )
  }

  onPortChange(item: AccessPoint, port: string, vlan: number | null) {
    if ((item.ports[port] ?? null) === vlan) return
    this.service.setPorts(item.name, { ...item.ports, [port]: vlan })
  }

  remove(item: AccessPoint) {
    this.dialogs
      .open<boolean>(TUI_CONFIRM, {
        label: this.i18n.transform('Remove Access Point'),
        data: {
          content: this.i18n.transform(
            'It stops serving its devices and waits to be adopted again.',
          ),
          yes: this.i18n.transform('Remove'),
          no: this.i18n.transform('Cancel'),
        },
      })
      .pipe(filter(Boolean))
      .subscribe(() => this.service.remove(item.name))
  }
}
