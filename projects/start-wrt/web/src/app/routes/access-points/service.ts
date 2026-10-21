import { inject, Injectable, signal } from '@angular/core'
import { FormService } from 'src/app/services/form.service'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { compareVersions } from 'src/app/utils/versions'
import {
  AccessPoint,
  AccessPointAdopt,
  AccessPointCandidate,
  ApiService,
  ProfileId,
} from 'src/app/services/api/api.service'

@Injectable()
export class AccessPointsService extends FormService<AccessPoint[]> {
  private readonly api = inject(ApiService)
  private readonly i18n = inject(i18nPipe)

  readonly candidates = signal<AccessPointCandidate[]>([])
  readonly profiles = signal<ProfileId[]>([])
  /** The main router's own firmware version, which an access point should match. */
  readonly mainVersion = signal('')

  async load(): Promise<AccessPoint[]> {
    const [list, candidates, profiles, info] = await Promise.all([
      this.api.accessPointList(),
      this.api.accessPointCandidates().catch(() => []),
      this.api.profilesList(),
      this.api.systemInfo().catch(() => null),
    ])

    this.candidates.set(candidates)
    this.profiles.set(profiles)
    if (info) this.mainVersion.set(info.version)

    return list
  }

  /** Running older firmware than the main router, or unable to read its configuration. */
  behind(item: AccessPoint): boolean {
    return item.needsUpdate || this.compare(item) < 0
  }

  newer(item: AccessPoint): boolean {
    return this.compare(item) > 0
  }

  /** Behind and not already queued or updating. */
  updatable(item: AccessPoint): boolean {
    return (
      this.behind(item) &&
      item.update?.state !== 'queued' &&
      item.update?.state !== 'updating'
    )
  }

  private compare(item: AccessPoint): number {
    const main = this.mainVersion()
    return item.version && main ? compareVersions(item.version, main) : 0
  }

  /** Names of access points out of contact: saved configurations a new
   *  router can take over (design §19). */
  saved(): string[] {
    return (this.data() || []).filter(s => !s.live).map(s => s.name)
  }

  async store(): Promise<void> {}

  adopt(params: AccessPointAdopt): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.accessPointAdopt(params)
        await this.refreshAndWait()
      },
      {
        loading: this.i18n.transform('Adopting...'),
        success: this.i18n.transform('Access point adopted'),
      },
    )
  }

  setPorts(
    name: string,
    ports: Record<string, number | null>,
  ): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.accessPointSetPorts({
          name,
          ports: Object.entries(ports).map(
            ([port, vlan]) => `${port}=${vlan === null ? 'off' : vlan}`,
          ),
        })
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Port assignments saved') },
    )
  }

  /** Updates `names`, or every access point behind the main router, one at a time. */
  update(names: string[]): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.accessPointUpdate({ names })
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Access point update started') },
    )
  }

  /** Deletes saved configurations, revoking each one's key. */
  removeSaved(names: string[]): Promise<boolean> {
    return this.actions.run(
      async () => {
        for (const name of names) {
          await this.api.accessPointUnpair({ name })
        }
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Saved configurations deleted') },
    )
  }

  remove(name: string): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.accessPointUnpair({ name })
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Access point removed') },
    )
  }
}
